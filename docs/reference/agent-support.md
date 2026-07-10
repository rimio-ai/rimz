# Agent support

RimZ watches the coding agents you already run — Claude Code, Codex, Pi, and OpenCode — so the question on install is a fair one: *will RimZ see my agent, and what exactly is it reading from it?* This page is the answer, and the compatibility matrix in the README is its one-line summary.

The answer is one uniform adapter per agent. An adapter translates that agent's own hooks, transcripts, and APIs into the vocabulary the rest of RimZ speaks, so `rimz agents` launches, `rimz message` steers, and `rimz agents … -p` scripts all four the same way. It reads what the agent does and classifies it; you answer in the agent's own UI, the CLI runs stock, and the official web, desktop, and mobile apps keep working untouched. The boundary in depth is [the agent model](../internals/agents/model.md).

Every integration is declared cell by cell, not assumed. Each adapter states its own coverage, conformance tests cross-check that declaration against the code that backs it, and `rimz coverage` prints the same matrix on demand — so what RimZ claims to read is a thing you verify on your own machine rather than take on faith:

```sh
rimz coverage          # the wired / partial / unsupported grid, per agent, with a reason on every cell
rimz coverage --json   # the same, machine-readable
```

This page is the annotated read of that command.

## Support at a glance

| Agent | Status | Coverage | Integration surface |
| --- | :---: | --- | --- |
| Claude Code | ✅ stable | 16 wired | hooks · statusline · `.jsonl` transcripts · `claude --resume` |
| Codex | ✅ stable | 11 wired · 2 derived · 3 unsupported | hooks + `notify` · app-server · rollout `.jsonl` · `codex resume` |
| Pi | beta | 7 wired · 3 derived · 6 unsupported | extension API · session `.jsonl` · `pi --session` |
| OpenCode | alpha | 8 wired · 3 derived · 5 unsupported | plugin API · session `.jsonl` + SQLite |

What the tiers promise:

- **✅ stable** — every product concern is carried by a native signal: live status, turn phase, task, context health, cost, subagents, resume, and blocking-ask routing all report end to end. This is the daily-driver path.
- **beta** — the integration is complete and in daily use, with a handful of fields reconstructed by derivation rather than pushed natively. Expect correct routing and state, and a rougher edge on enrichment.
- **alpha** — the integration works and reports live state, with the widest set of derived cells. Use it, and expect the surface to keep filling in as the agent exposes more.

Every tier delivers the core promise: the agent appears in the sidebar, its blocking questions route to your keyboard, and you answer in its own UI. The tier grades how much of the enrichment around that is native versus derived.

## The coverage matrix

`rimz coverage` scores each agent against sixteen product concerns. A cell reads **wired** (✓, a native signal carries it directly), **partial** (◐, no native signal, so RimZ reconstructs the behaviour from other state), or **unsupported** (✗, unreachable from the agent's current protocol). A partial cell still shows you a live figure — it trades a native push for a derivation, and the command names the exact gap that derivation leaves.

| Concern | Claude | Codex | Pi | OpenCode | What it drives |
| --- | :--: | :--: | :--: | :--: | --- |
| `turn` | ✓ | ✓ | ✓ | ✓ | live status — session start and every turn boundary |
| `perm` | ✓ | ✓ | ✓ | ✓ | permission prompts routed to your keyboard |
| `plan` | ✓ | ✗ | ✗ | ✗ | a plan-approval gate raises a waiting row |
| `ask` | ✓ | ✓ | ✗ | ✗ | the agent's ask-the-user tool raises a waiting row |
| `answer` | ✓ | ✗ | ✗ | ✗ | structured answers drive supported native prompt actions |
| `compact` | ✓ | ✓ | ✓ | ✓ | context compaction shows on the card |
| `sub` | ✓ | ✓ | ✗ | ✓ | the subagent tree renders as nested rows |
| `bg` | ✓ | ✗ | ✗ | ✗ | a turn parked on background work stays tracked |
| `end` | ✓ | ◐ | ✓ | ◐ | the card tombstones when the session closes |
| `idle` | ✓ | ◐ | ◐ | ◐ | an idle nudge when the agent goes quiet on you |
| `usage` | ✓ | ✓ | ✓ | ✓ | context-window fill and token counts |
| `live$` | ✓ | ✓ | ◐ | ◐ | the live dollar figure on the card |
| `rich` | ✓ | ✓ | ◐ | ✓ | provider extras — official model labels, account windows |
| `install` | ✓ | ✓ | ✓ | ✓ | RimZ can install the reporting hooks |
| `spend` | ✓ | ✓ | ✓ | ✓ | account spend for the [token-insight](../guide/insight.md) dashboard |
| `remote` | ✓ | ✓ | ✗ | ✗ | drive or spawn a session with no local pane |

<sub>✓ wired · ◐ partial (derived) · ✗ unsupported. Run `rimz coverage` for the live grid with the exact reason printed on every ◐ and ✗ cell.</sub>

The columns thin out from left to right for a reason: Claude Code is the reference integration and carries every concern natively, and each other agent exposes less of its internals to a local observer. A ✗ is an honest declared absence — the sidebar and `rimz doctor` read the same declaration, so a missing surface renders as a stated gap rather than a silent bug.

## Per-agent detail

### Claude Code

Claude is the reference integration and the fullest one, wired on all sixteen concerns. Its hooks run **standalone** — in the agent's own pane — so every event stamps the pane directly and identity binding is exact and free ([the instance lifecycle](../internals/agents/model.md#the-instance-lifecycle)).

- **Reports:** session start and end, every turn boundary, per-tool activity, subagents (the `Task` tool tree, each child as its own nested row), plan approvals, and permission prompts. `SessionEnd` tombstones the row the moment the session closes.
- **Structured answers:** `rimz answer` drives `AskUserQuestion` picks, multi-select, and free text, permission `allow`, and caution-marked plan `approve`. Denial, persistent grants, keep-planning, refinement text, and manual-review approval stay in the Claude pane because their controls carry no confirming lifecycle event.
- **Live context:** Claude's statusline is wrapped to push rich per-session data — context window, usage, cost, model display name — on its own cadence, so the card's context meter and dollar figure stay live rather than turn-grained. The context window is read from the model id, and a `[1m]` capability tag widens it.
- **Resume and fork:** `claude --resume` reopens a session; adding `--fork-session` branches one for `rimz agents fork`.
- **Permission modes:** `claude-{auto,ask,plan,yolo,ping}` as launch cells; on the command line `--ask` keeps Claude's own prompts and `--yolo` passes `--dangerously-skip-permissions`. Effort levels: `low|medium|high|xhigh|max`.
- **Account:** probed with `claude auth status` for plan and login state, feeding the provider dashboard.
- **Install target:** `~/.claude/settings.json`, edited additively and reversed exactly by `rimz hooks uninstall`.

Mapping detail: [claude.md](../internals/agents/claude.md); upstream protocol: [claude-reference.md](../externals/agent-adapter/claude-reference.md).

### Codex

Codex is a full integration with one structural difference: since 0.137 its hooks are **daemon-routed**, firing from a shared per-user app-server rather than the pane. RimZ recovers the pane from the in-pane `codex` process at the same working directory, so an in-pane Codex session still renders as a normal, jump-able row ([hooks resolve the room they live in](../internals/agents/model.md#hooks-resolve-the-room-they-live-in)).

- **Reports:** turn boundaries, per-tool activity, subagents (thread-spawned children since 0.134), plan approvals, permission prompts, and the `notify` channel.
- **Two derived cells (◐):** `end` — Codex has no per-session end hook, so a closed session leaves by pane liveness and the rollup reaper on the next snapshot tick rather than at the instant of exit; `idle` — reconstructed from turn-end, `request_user_input`, and the stall window, without a native idle-timeout nudge.
- **Three unsupported cells (✗):** `plan` — no plan-approval gate, since `update_plan` is non-blocking (`codex-plan` keeps the default posture); `answer` — no mapped prompt choreography; `bg` — no background-task parking.
- **Live context:** the rollout `.jsonl` tail is the native live source for tokens, cost, and effort, read under a stat gate so an unchanged file costs nothing; the read-only app-server methods supply account and rate-limit context. The context window comes from the rollout's `model_context_window`.
- **Turn-death handling:** Codex can end a turn on a provider limit with no error record and no `Stop` hook. RimZ confirms these from a bounded pane capture plus the account budget, so a paused Codex reads as `⏸` rather than a false success or a stall ([turn-completion and turn-death markers](../internals/agents/codex.md#turn-completion-marker)); `rimz agents refresh @codex` re-runs the check on demand.
- **Resume and fork:** `codex resume` reopens a session; `codex fork <id>` branches one for `rimz agents fork`.
- **Permission modes:** `codex-{auto,ask,plan,yolo,ping}` as launch cells; on the command line `--yolo` passes `--dangerously-bypass-approvals-and-sandbox`. Effort levels: `minimal|low|medium|high|xhigh`.
- **Install target:** `~/.codex/config.toml`.

Mapping detail: [codex.md](../internals/agents/codex.md); upstream protocol: [codex-reference.md](../externals/agent-adapter/codex-reference.md).

### Pi

Pi runs in-process in its pane and reports through a RimZ-authored **extension**, which stamps the context gauge directly onto each hook envelope — so Pi carries live context on the lifecycle channel with no transcript tail or separate transport.

- **Reports:** turn boundaries, per-tool activity, and context usage on every envelope.
- **Three derived cells (◐):** `idle` — turn-end plus the stall window, without a native idle-timeout nudge; `live$` — the extension pushes a cumulative-cost figure and a turn-end walk sums the session transcript spend, so the in-process accumulator is best-effort and resets on resume while the turn-end walk reconciles to the authoritative session total; `rich` — the extension envelope carries model, effort, cost, and account windows, but rides the lifecycle channel with no out-of-band transport refreshing it between turns, unlike a statusline or app-server poll.
- **Six unsupported cells (✗):** `plan` (no plan-approval gate), `ask` (no native question tool), `answer` (no mapped prompt choreography), `sub` (no subagent hook surface), `bg` (no background-task parking), and `remote` (no remote-control surface).
- **Blocking asks:** Pi has no native prompt UI, so its adapter declares `native_ask_ui` off. A blocking hook returns the neutral no-op — which for Pi *is* the allow — and RimZ records no waiting row, because there is no native prompt a `?` row could route you to. Pi's neutral semantics differ from Claude's and Codex's, so verify this per agent.
- **Resume and fork:** `pi --session` reopens a session; `pi --fork <id>` branches one for `rimz agents fork`.
- **Permission modes:** `pi-{ask,plan}` as launch cells. Effort levels: `off|minimal|low|medium|high|xhigh`.
- **Account:** read from `~/.pi/agent/auth.json` — OAuth is a metered subscription, an API key is unmetered.
- **Install target:** `~/.pi/agent/extensions/rimz.ts`.

Mapping detail: [pi.md](../internals/agents/pi.md); upstream protocol: [pi-reference.md](../externals/agent-adapter/pi-reference.md).

### OpenCode

OpenCode reports through a RimZ-authored **plugin** that maintains its context gauge from the agent's `message.updated` events and stamps the latest usage split, plus the model's context window from OpenCode's own catalog, onto each lifecycle envelope. Interactive OpenCode runs in-process in its pane and binds standalone.

- **Reports:** turn boundaries, per-tool activity, subagents, and context usage per envelope.
- **Three derived cells (◐):** `end` — `dispose` is server-scoped and carries no session id, so the card leaves on pane liveness and the reaper rather than at exit; `idle` — turn-end, `permission.ask`, and the stall window, without a native idle-timeout nudge; `live$` — summed from the session store (SQLite) at turn end, a reconstructed figure rather than a provider-pushed realtime one.
- **Five unsupported cells (✗):** `plan` (no plan-approval gate), `ask` (the question tool has no contracted bus event in 1.15.13), `answer` (no mapped prompt choreography), `bg` (no background-task parking), and `remote` (no remote-control surface).
- **Rich context:** the embedded server's `/config/providers` and `/session` endpoints, reached over the plugin's `serverUrl`, supply official model labels and account windows.
- **Resume and fork:** `opencode --session <id>` reopens a session; adding `--fork` branches one for `rimz agents fork`.
- **Permission modes:** no launch-cell suffixes yet — launch OpenCode bare or through a profile (`rimz agents --help` lists the current flags).
- **Account:** read from OpenCode's own `auth.json` — the probe reports the active provider, with OAuth as a metered subscription and an API key as unmetered. OpenCode exposes no plan tier or quota surface of its own, so usage bars come from the backing provider's endpoints, the same Anthropic and ChatGPT probes Claude and Codex use.
- **Install target:** `~/.config/opencode/plugin/rimz.ts`.

Mapping detail: [opencode.md](../internals/agents/opencode.md); upstream protocol: [opencode-reference.md](../externals/agent-adapter/opencode-reference.md).

## The lifecycle hook surface

Under the concern matrix sits the raw event surface: the eleven lifecycle signals RimZ folds into every agent's state machine, and the native event each agent fires for each one. `rimz coverage` prints this as its second grid, the hooks matrix; here it is with the native event names in place.

| Signal | Claude | Codex | Pi | OpenCode |
| --- | --- | --- | --- | --- |
| `registered` | `SessionStart` | `SessionStart` | `session_start` | `session_created` |
| `turn_started` | `UserPromptSubmit` | `UserPromptSubmit` | `before_agent_start` | `chat_message` |
| `turn_ended` | `Stop` | `Stop` | `agent_end` | `session_idle` |
| `tool_used` | `PostToolUse` | `PostToolUse` | `tool_execution_end` | `tool_after` |
| `awaiting_input` | `PermissionRequest` | `PermissionRequest` | ✗ | `permission_ask` |
| `subagent_started` | `SubagentStart` | `SubagentStart` | ✗ | `SubagentStart` |
| `subagent_stopped` | `SubagentStop` | `SubagentStop` | ✗ | `SubagentStop` |
| `compacting` | `PreCompact` | `PreCompact` | `session_before_compact` | `session_compacting` |
| `compaction_ended` | `PostCompact` | `PostCompact` | `session_compact` | `session_compacted` |
| `ended` | `SessionEnd` | ◐ derived | `session_shutdown` | ◐ derived |
| `lost` | ◐ derived | ◐ derived | ◐ derived | ◐ derived |

`lost` — an agent's mux-session dying out from under it — has no native event in any of the four, because an agent's own hooks stop firing exactly when the thing that would report the death is gone. RimZ derives it from the `rimz exec` launch wrapper instead. Where `ended` is derived (Codex, OpenCode), the same pane-liveness-and-reaper path clears the row on the next snapshot tick rather than at the instant of exit.

## Versions

RimZ tracks each agent's own release surface, and behaviour can shift with the agent's version — Codex, for example, moved to daemon-routed hooks at 0.137 and adjusted turn-completion signals through the 0.14x line. RimZ adapts to these at runtime rather than pinning a hard floor in this page, and `rimz doctor` reports any version drift it detects per agent after an upgrade ([troubleshooting](../guide/troubleshooting.md)). For the exact event surface a given agent version exposes, the authority is that agent's [adapter doc](../internals/agents/model.md) and [external reference](../externals/agent-adapter/claude-reference.md).

## How adapters work

Adding an agent is implementing one trait plus a descriptor and a single registry line; nothing else in RimZ changes, because status, ranking, liveness, cost, and blocking-ask routing all flow from the agent-agnostic observation the adapter emits. Two invariants keep the seam honest: the adapter only classifies and normalizes, leaving every store write to the hook runner, and downstream code reads only that normalized observation. The full boundary, the two hook channels, and the install mechanics are in [the agent model](../internals/agents/model.md#the-adapter-boundary).

Installing an agent's hooks edits that agent's own config — the install target listed for each agent above — so it is a visible, consented step. `rimz start` offers it on first run with a diff preview, `rimz hooks install --dry-run` prints the patch and writes nothing, and `rimz hooks uninstall` reverses it exactly ([hooks and trust reference](./cli/hooks-trust.md)). The install is additive, so your existing hooks stay. An agent run before its hooks are installed simply reports nothing — it stays invisible in the sidebar rather than half-working, and one `rimz hooks install` wires it in.

## Agents not yet supported

An agent RimZ doesn't recognize runs fine in a pane; it renders as a plain process row rather than an agent card, with no live state or attention routing. New agents such as Cursor, Gemini, or Copilot land the same way the four here did — one adapter over their verified hook surface ([adding an agent](../internals/agents/model.md#adding-an-agent)). Two other categories are known gaps: **remote agents** with no local pane (a `claude remote-control --spawn` worktree, or a Codex thread started from the web) are tracked but not yet rendered, and an agent whose hooks you declined at the consent gate reports nothing until you wire it with `rimz hooks install`.

## See also

- [Agents](../guide/agents.md) — launching agents and profiles across every supported kind.
- [Teams](../guide/teams.md) — pairing models by role across supported kinds.
- [Messaging](../guide/messaging.md) — steering and queuing agents by handle.
- [Token insight](../guide/insight.md) — where the `live$` and `spend` figures surface, and how each is calculated.
- [The agent model](../internals/agents/model.md) — the rollup, state machine, and adapter boundary in depth.
- [Configuration](../guide/configuration.md#agent-profiles-commands-and-teams) — profiles, effort, and per-agent launch args.
- [Troubleshooting](../guide/troubleshooting.md) — `rimz doctor`, hooks not reporting, and version drift.
