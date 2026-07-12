# Agent support

RimZ watches the coding agents you already run — Claude Code, Codex, Copilot, Gemini CLI, Pi, OpenCode, and Cursor — so the question on install is a fair one: *will RimZ see my agent, and what exactly is it reading from it?* This page is the answer, and the compatibility matrix in the README is its one-line summary.

The answer is one uniform adapter per agent. An adapter translates that agent's own hooks, transcripts, and APIs into the vocabulary the rest of RimZ speaks, so `rimz agents` launches, `rimz message` steers, and `rimz agents … -p` scripts every built-in that exposes it through the same boundary. It reads what the agent does and classifies it; you answer in the agent's own UI, the CLI runs stock, and the official web, desktop, and mobile apps keep working untouched. The boundary in depth is [the agent model](../internals/agents/model.md).

Third-party agents use the same boundary through a machine-tier process plugin. Its manifest derives the same matrices and its shim speaks the [canonical agent plugin protocol](./agent-plugins.md); feature status is bundle-specific rather than assigned a RimZ release tier.

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
| Copilot | alpha | 5 wired · 2 derived · 9 unsupported | command hooks · `copilot --resume` |
| Gemini CLI | beta | 7 wired · 4 derived · 5 unsupported | hooks · session `.jsonl` · `gemini --resume` |
| Pi | beta | 7 wired · 3 derived · 6 unsupported | extension API · session `.jsonl` · `pi --session` |
| OpenCode | alpha | 8 wired · 3 derived · 5 unsupported | plugin API · session `.jsonl` + SQLite |
| Cursor | alpha | 4 wired · 2 derived · 10 unsupported | command hooks · opaque transcript metadata · `agent --resume` |
| Third-party plugin | bundle-defined | derived by `rimz coverage` | canonical event shim · optional executable probes |

What the tiers promise:

- **✅ stable** — every product concern is carried by a native signal: live status, turn phase, task, context health, cost, subagents, resume, and blocking-ask routing all report end to end. This is the daily-driver path.
- **beta** — the integration is complete and in daily use, with a handful of fields reconstructed by derivation rather than pushed natively. Expect correct routing and state, and a rougher edge on enrichment.
- **alpha** — the integration works and reports live state, with the widest set of derived cells. Use it, and expect the surface to keep filling in as the agent exposes more.

Every tier delivers the core promise where the agent exposes the required local signals. Cursor appears and reports work, but its stock local hooks cannot report that a native question is open, so it intentionally has no waiting row or ask routing.

## The coverage matrix

`rimz coverage` scores each agent against sixteen product concerns. A cell reads **wired** (✓, native signals carry the full concern), **partial** (◐, native coverage is incomplete and RimZ reconstructs the rest from another signal or state), or **unsupported** (✗, unreachable from the agent's current protocol). A partial cell still shows you a live figure, and the command names the exact gap that derivation leaves.

One row per agent, so a new agent adds exactly one line:

| Agent | `turn` | `perm` | `plan` | `ask` | `answer` | `compact` | `sub` | `bg` | `end` | `idle` | `usage` | `live$` | `rich` | `install` | `spend` | `remote` |
| --- | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: |
| Claude | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Codex | ✓ | ✓ | ✗ | ✓ | ✗ | ✓ | ✓ | ✗ | ◐ | ◐ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Copilot | ✓ | ✓ | ✗ | ✓ | ✗ | ◐ | ✗ | ✗ | ✓ | ◐ | ✗ | ✗ | ✗ | ✓ | ✗ | ✗ |
| Gemini | ✓ | ✓ | ✓ | ✓ | ✗ | ◐ | ✗ | ✗ | ✓ | ◐ | ✓ | ◐ | ✗ | ✓ | ◐ | ✗ |
| Pi | ✓ | ✓ | ✗ | ✗ | ✗ | ✓ | ✗ | ✗ | ✓ | ◐ | ✓ | ◐ | ◐ | ✓ | ✓ | ✗ |
| OpenCode | ✓ | ✓ | ✗ | ✓ | ✗ | ✓ | ✓ | ✗ | ◐ | ◐ | ✓ | ◐ | ✓ | ✓ | ✓ | ✗ |
| Cursor | ✓ | ✗ | ✗ | ✗ | ✗ | ◐ | ✗ | ✗ | ✓ | ◐ | ✓ | ✗ | ✗ | ✓ | ✗ | ✗ |

<sub>✓ wired · ◐ partial (derived) · ✗ unsupported. Run `rimz coverage` for the live grid with the exact reason printed on every ◐ and ✗ cell.</sub>

What each concern column drives: `turn` live status (session start and every turn boundary), `perm` permission prompts routed to your keyboard, `plan` a plan-approval gate raising a waiting row, `ask` the agent's ask-the-user tool raising a waiting row, `answer` structured answers driving supported native prompt actions, `compact` context compaction on the card, `sub` the subagent tree as nested rows, `bg` a turn parked on background work, `end` the card tombstoning when the session closes, `idle` an idle nudge when the agent goes quiet, `usage` context-window fill and token counts, `live$` the live dollar figure, `rich` provider extras (official model labels, account windows), `install` RimZ installing the reporting hooks, `spend` account spend for the [token-insight](../guide/insight.md) dashboard, and `remote` driving or spawning a session with no local pane.

The rows thin out from top to bottom for a reason: Claude Code is the reference integration and carries every concern natively, and each other agent exposes less of its internals to a local observer. A ✗ is an honest declared absence — the sidebar and `rimz doctor` read the same declaration, so a missing surface renders as a stated gap rather than a silent bug.

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

- **Reports:** turn boundaries, supported-tool activity, subagents (thread-spawned children since 0.134), permission prompts, and native user questions. Codex has no plan-approval hook; `notify` is a separate upstream channel that RimZ does not install.
- **Two derived cells (◐):** `end` — Codex has no per-session end hook, so a closed session leaves by pane liveness and the rollup reaper on the next snapshot tick rather than at the instant of exit; `idle` — reconstructed from turn-end, `request_user_input`, and the stall window, without a native idle-timeout nudge.
- **Three unsupported cells (✗):** `plan` — no plan-approval gate, since `update_plan` is non-blocking (`codex-plan` keeps the default posture); `answer` — no mapped prompt choreography; `bg` — no background-task parking.
- **Live context:** the rollout `.jsonl` tail is the native live source for tokens, cost, and effort, read under a stat gate so an unchanged file costs nothing; the read-only app-server methods supply account and rate-limit context. The context window comes from the rollout's `model_context_window`.
- **Turn-death handling:** Codex can end a turn on a provider limit with no error record and no `Stop` hook. RimZ confirms these from a bounded pane capture plus the account budget, so a paused Codex reads as `⏸` rather than a false success or a stall ([turn-completion and turn-death markers](../internals/agents/codex.md#turn-completion-marker)); `rimz agents refresh @codex` re-runs the check on demand.
- **Resume and fork:** `codex resume` reopens a session; `codex fork <id>` branches one for `rimz agents fork`.
- **Permission modes:** `codex-{auto,ask,plan,yolo,ping}` as launch cells; on the command line `--yolo` passes `--dangerously-bypass-approvals-and-sandbox`. Effort levels: `minimal|low|medium|high|xhigh`.
- **Install target:** `~/.codex/config.toml`.

Mapping detail: [codex.md](../internals/agents/codex.md); upstream protocol: [codex-reference.md](../externals/agent-adapter/codex-reference.md).

### Copilot

Copilot reports through native camelCase command hooks in one RimZ-owned user hook file. The adapter is hooks-only: lifecycle and native blocking prompts work, while unpublished enrichment and local-store schemas remain visibly unsupported.

- **Reports:** session and turn boundaries, mutating-tool activity, permission prompts, native `ask_user` questions, compaction start, and non-recoverable error markers.
- **Two derived cells (◐):** `compact` — `preCompact` opens a bracket that the next lifecycle signal closes because hooks expose no post-compact event; `idle` — reconstructed from `agentStop` plus the stall window without the unwired notification event.
- **Nine unsupported cells (✗):** plan approval, structured answers, subagents, background parking, context usage, realtime cost, rich context, account spend, and remote control remain outside the documented hook surface or lack stable identity/schema.
- **Resume:** `copilot --resume <id>` restores a session. Fork stays interactive-only upstream.
- **Prompt launches:** `copilot --interactive <prompt>` starts the stock interactive UI and submits the initial prompt, so prompt-seeded panes and supervised `rimz agents copilot -p` retain native asks and hook-driven completion.
- **Permission modes:** ask adds no flag, plan uses `--plan`, auto uses `--autopilot`, and yolo uses `--allow-all`. Model and effort profiles use `--model` and `--effort`.
- **Install target:** `~/.copilot/hooks/rimz.json`, owned whole-file through the first-line `_rimz_managed` marker.

Mapping detail: [copilot.md](../internals/agents/copilot.md); upstream protocol: [copilot-reference.md](../externals/agent-adapter/copilot-reference.md).

### Gemini CLI

Gemini reports through stock command hooks that run as children of the interactive pane, so session registration and pane binding are direct. Its project-scoped JSONL transcript supplies the current model, context use, and token-priced spend.

- **Reports:** session start and end, turn boundaries, completed mutating tools, permission notifications, native questions, plan approval, and compaction start.
- **Four derived cells (◐):** `compact` closes on the next lifecycle signal because Gemini has no post-compress hook; `idle` combines turn boundaries, ask paths, and the stall window; `live$` is reconstructed from transcript tokens at turn end; `spend` has transcript history and local login identity but no Code Assist quota window.
- **Five unsupported cells (✗):** `answer` (native TUI choreography is not mapped), `sub` (child hooks are not live-verified), `bg` (no background-task parking), `rich` (no provider-owned rich context channel), and `remote` (ACP owns a new stdio session rather than observing a running TUI).
- **Context:** the newest active Gemini message's `tokens.total`, after message replacement, checkpoints, and rewinds; current Gemini routes use a 1,048,576-token window and Gemma uses 256,000.
- **Resume and fork:** `gemini --resume <id>` reopens a session; Gemini exposes no native fork.
- **Permission modes:** `gemini-{auto,ask,plan,yolo}` map to `--approval-mode`; model profiles use `--model`, while effort and system-prompt files are unsupported.
- **Account:** `security.auth.selectedType` names the method and `google_accounts.json.active` labels Google OAuth without reading secrets. The internal Code Assist quota probe is deferred.
- **Install target:** `~/.gemini/settings.json`, merged additively; uninstall removes only commands containing `rimz hooks feed --source gemini`.

Mapping detail: [gemini.md](../internals/agents/gemini.md); upstream protocol: [gemini-reference.md](../externals/agent-adapter/gemini-reference.md).

### Pi

Pi runs in-process in its pane and reports through a RimZ-authored **extension**, which stamps the context gauge directly onto each hook envelope — so Pi carries live context on the lifecycle channel with no transcript tail or separate transport.

- **Reports:** turn boundaries, per-tool activity, and context usage on every envelope.
- **Three derived cells (◐):** `idle` — native final-idle `agent_settled` plus the stall window, without an idle-timeout nudge; `live$` — the extension pushes a cumulative-cost figure and a settled-boundary walk sums the session transcript spend, so the in-process accumulator is best-effort and resets on resume while the walk reconciles to the authoritative session total; `rich` — the extension envelope carries model, effort, cost, and account windows, but rides the lifecycle channel with no out-of-band transport refreshing it between turns, unlike a statusline or app-server poll.
- **Six unsupported cells (✗):** `plan` (no plan-approval gate), `ask` (no native question tool), `answer` (no mapped prompt choreography), `sub` (no subagent hook surface), `bg` (no background-task parking), and `remote` (no remote-control surface).
- **Blocking asks:** Pi has no native prompt UI, so its adapter declares `native_ask_ui` off. A blocking hook returns the neutral no-op — which for Pi *is* the allow — and RimZ records no waiting row, because there is no native prompt a `?` row could route you to. Pi's neutral semantics differ from Claude's and Codex's, so verify this per agent.
- **Resume and fork:** `pi --session` reopens a session; `pi --fork <id>` branches one for `rimz agents fork`.
- **Permission modes:** `pi-{ask,plan}` as launch cells. Effort levels: `off|minimal|low|medium|high|xhigh|max` (`max` requires Pi 0.80.6+ and supporting models).
- **Account:** read from `~/.pi/agent/auth.json` — OAuth is a metered subscription, an API key is unmetered.
- **Install target:** `~/.pi/agent/extensions/rimz.ts`.

Mapping detail: [pi.md](../internals/agents/pi.md); upstream protocol: [pi-reference.md](../externals/agent-adapter/pi-reference.md).

### OpenCode

OpenCode reports through a RimZ-authored **plugin** that maintains its context gauge from the agent's `message.updated` events and stamps the latest usage split, plus the model's context window from OpenCode's own catalog, onto each lifecycle envelope. Interactive OpenCode runs in-process in its pane and binds standalone.

- **Reports:** turn boundaries, per-tool activity, subagents, permission prompts, native user questions, and context usage per envelope.
- **Three derived cells (◐):** `end` — `dispose` is server-scoped and carries no session id, so the card leaves on pane liveness and the reaper rather than at exit; `idle` — turn-end, native permission/question prompts, and the stall window, without a native idle-timeout nudge; `live$` — summed from the session store (SQLite) at turn end, a reconstructed figure rather than a provider-pushed realtime one.
- **Four unsupported cells (✗):** `plan` (no plan-approval gate), `answer` (no mapped prompt choreography), `bg` (no background-task parking), and `remote` (no remote-control surface).
- **Rich context:** the embedded server's `/config/providers` and `/session` endpoints, reached over the plugin's `serverUrl`, supply official model labels and account windows.
- **Resume and fork:** `opencode --session <id>` reopens a session; adding `--fork` branches one for `rimz agents fork`.
- **Permission modes:** no launch-cell suffixes yet — launch OpenCode bare or through a profile (`rimz agents --help` lists the current flags).
- **Account:** read from OpenCode's own `auth.json` — the probe reports the active provider, with OAuth as a metered subscription and an API key as unmetered. OpenCode exposes no plan tier or quota surface of its own, so usage bars come from the backing provider's endpoints, the same Anthropic and ChatGPT probes Claude and Codex use.
- **Install target:** `~/.config/opencode/plugin/rimz.ts`.

Mapping detail: [opencode.md](../internals/agents/opencode.md); upstream protocol: [opencode-reference.md](../externals/agent-adapter/opencode-reference.md).

### Cursor

Cursor reports through its native command hooks and runs standalone in its pane. RimZ discovers `cursor-agent` before `agent`; it never probes `cursor`, which is the IDE executable.

- **Reports:** session start and end, turn boundaries, mutating tool completion, compaction start, and the context gauge carried by `preCompact`.
- **Two derived cells (◐):** `compact` — `preCompact` opens the bracket and the next lifecycle signal closes it because Cursor exposes no post-compaction event; `idle` — reconstructed from turn boundaries and the stall window without an idle notification.
- **Ten unsupported cells (✗):** every ask/answer concern, subagents, background parking, realtime cost, rich context, account spend, and remote control. Cursor's local hook catalog has no permission, question, or plan-approval event, so RimZ never paints a false waiting row; answer the native prompt in Cursor's pane.
- **Context and transcript:** `preCompact` supplies context percentage and window size. Cursor publishes a transcript path but no transcript schema, so RimZ carries the path as metadata and does not parse it.
- **Resume and fork:** `agent --resume <id>` reopens a conversation; Cursor exposes `/fork` interactively but no CLI-by-id fork surface.
- **Permission modes:** Ask uses Cursor's default; Plan passes `--mode=plan`; Auto passes `--auto-review`; Yolo passes `--force --sandbox disabled`. Neutral-hook behavior under each posture remains a live-verification item.
- **Cost and account:** no documented machine-readable token, dollar, quota, or account schema exists, so the adapter does not scrape the terminal or guess at opaque JSON.
- **Install target:** `~/.cursor/hooks.json`, merged additively; every installed lifecycle hook returns neutral `{}` JSON.

Mapping detail: [cursor.md](../internals/agents/cursor.md); upstream protocol: [cursor-reference.md](../externals/agent-adapter/cursor-reference.md).

## The lifecycle hook surface

Under the concern matrix sits the raw event surface: the eleven lifecycle signals RimZ folds into every agent's state machine, and the native event each agent fires for each one. `rimz coverage` prints this as its second grid, the hooks matrix; here it is with the native event names in place, one row per agent so a new agent adds a single line.

| Agent | `registered` | `turn_started` | `turn_ended` | `tool_used` | `awaiting_input` | `subagent_started` | `subagent_stopped` | `compacting` | `compaction_ended` | `ended` | `lost` |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Claude | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `PermissionRequest` | `SubagentStart` | `SubagentStop` | `PreCompact` | `PostCompact` | `SessionEnd` | ◐ derived |
| Codex | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `PermissionRequest` | `SubagentStart` | `SubagentStop` | `PreCompact` | `PostCompact` | ◐ derived | ◐ derived |
| Copilot | `sessionStart` | `userPromptSubmitted` | `agentStop` | `postToolUse` | `permissionRequest` | ✗ | ✗ | `preCompact` | ◐ derived | `sessionEnd` | ◐ derived |
| Gemini | `SessionStart` | `BeforeAgent` | `AfterAgent` | `AfterTool` | `Notification` | ✗ | ✗ | `PreCompress` | ◐ derived | `SessionEnd` | ◐ derived |
| Pi | `session_start` | `before_agent_start` | `agent_settled` (`agent_end` before Pi 0.80.4) | `tool_execution_end` | ✗ | ✗ | ✗ | `session_before_compact` | `session_compact` | `session_shutdown` | ◐ derived |
| OpenCode | `session_created` | `chat_message` | `session_idle` | `tool_after` | `permission_ask` | `SubagentStart` | `SubagentStop` | `session_compacting` | `session_compacted` | ◐ derived | ◐ derived |
| Cursor | `sessionStart` | `beforeSubmitPrompt` | `stop` | `postToolUse` | ✗ | ✗ | ✗ | `preCompact` | ◐ derived | `sessionEnd` | ◐ derived |

`lost` — an agent's mux-session dying out from under it — has no native event in any built-in, because an agent's own hooks stop firing exactly when the thing that would report the death is gone. RimZ derives it from the `rimz exec` launch wrapper instead. Where `ended` is derived (Codex, OpenCode), the same pane-liveness-and-reaper path clears the row on the next snapshot tick rather than at the instant of exit.

## Versions

RimZ tracks each agent's own release surface, and behaviour can shift with the agent's version — Codex, for example, moved to daemon-routed hooks at 0.137 and adjusted turn-completion signals through the 0.14x line. RimZ adapts to these at runtime rather than pinning a hard floor in this page, and `rimz doctor` reports any version drift it detects per agent after an upgrade ([troubleshooting](../guide/troubleshooting.md)). For the exact event surface a given agent version exposes, the authority is that agent's [adapter doc](../internals/agents/model.md) and [external reference](../externals/agent-adapter/claude-reference.md).

## How adapters work

Adding an agent is implementing one trait plus a descriptor and a single registry line; nothing else in RimZ changes, because status, ranking, liveness, cost, and blocking-ask routing all flow from the agent-agnostic observation the adapter emits. Two invariants keep the seam honest: the adapter only classifies and normalizes, leaving every store write to the hook runner, and downstream code reads only that normalized observation. The full boundary, the two hook channels, and the install mechanics are in [the agent model](../internals/agents/model.md#the-adapter-boundary).

Installing an agent's hooks edits that agent's own config — the install target listed for each agent above — so it is a visible, consented step. `rimz start` offers it on first run with a diff preview, `rimz hooks install --dry-run` prints the patch and writes nothing, and `rimz hooks uninstall` reverses it exactly ([hooks and trust reference](./cli/hooks-trust.md)). The install is additive, so your existing hooks stay. An agent run before its hooks are installed simply reports nothing — it stays invisible in the sidebar rather than half-working, and one `rimz hooks install` wires it in.

## Agents not yet supported

An agent RimZ doesn't recognize runs fine in a pane; it renders as a plain process row rather than an agent card, with no live state or attention routing. New agents such as Amp or Droid land the same way the built-ins here did — one adapter over their verified hook surface ([adding an agent](../internals/agents/model.md#adding-an-agent)). Two other categories are known gaps: **remote agents** with no local pane (a `claude remote-control --spawn` worktree, or a Codex thread started from the web) are tracked but not yet rendered, and an agent whose hooks you declined at the consent gate reports nothing until you wire it with `rimz hooks install`.

## See also

- [Agents](../guide/agents.md) — launching agents and profiles across every supported kind.
- [Teams](../guide/teams.md) — pairing models by role across supported kinds.
- [Messaging](../guide/messaging.md) — steering and queuing agents by handle.
- [Token insight](../guide/insight.md) — where the `live$` and `spend` figures surface, and how each is calculated.
- [The agent model](../internals/agents/model.md) — the rollup, state machine, and adapter boundary in depth.
- [Configuration](../guide/configuration.md#agent-profiles-commands-and-teams) — profiles, effort, and per-agent launch args.
- [Troubleshooting](../guide/troubleshooting.md) — `rimz doctor`, hooks not reporting, and version drift.
