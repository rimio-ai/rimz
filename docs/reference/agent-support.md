# Agent support

Rimz reaches four coding agents — Claude Code, Codex, Pi, and OpenCode — through one uniform adapter layer, and each agent runs stock in its own pane with the official web, desktop, and mobile apps untouched. An adapter is a thin translator over the agent's own hooks, transcripts, and APIs: it reports what the agent is doing and never acts or answers for you. Because everything downstream of the adapter is agent-agnostic, one interface drives all four — `rimz agents` launches them, `rimz message` steers them, and `rimz agents … -p` scripts them the same way regardless of kind. This page is the per-agent detail behind the README's compatibility matrix; the model that unifies them is [the agent model](../internals/agents/model.md).

## Status matrix

| Agent | Status | Integration surface |
| --- | :---: | --- |
| Claude Code | ✅ stable | hooks · statusline · `.jsonl` transcripts · `claude --resume` |
| Codex | ✅ stable | hooks + `notify` · app-server · rollout `.jsonl` · `codex resume` |
| Pi | beta | extension API · session `.jsonl` · `pi --session` |
| OpenCode | alpha | extension API · session `.jsonl` |

What the tiers promise:

- **✅ stable** — the full lifecycle is wired through native hooks: live status, turn phase, task, context health, cost, subagents, resume, and blocking-ask routing all report end to end. This is the daily-driver path.
- **beta** — the integration is complete and in daily use, with one or more fields reconstructed rather than natively pushed (Pi declares partial live cost). Expect correct routing and state; expect a rougher edge on enrichment.
- **alpha** — the integration works and reports live state, with the widest set of derived-not-native fields (OpenCode reconstructs session end, idle, and live cost). Use it, and expect the surface to keep filling in.

Every tier delivers the core promise: the agent appears in the sidebar, its blocking questions route to your keyboard, and you answer in its own UI. The tier grades how much of the enrichment around that is native versus derived.

## Per-agent detail

### Claude Code

Claude is the reference integration and the fullest one. Its hooks run **standalone** — in the agent's own pane — so every event stamps the pane directly and identity binding is exact and free ([the instance lifecycle](../internals/agents/model.md#the-instance-lifecycle)).

- **What it reports:** session start and end, every turn boundary, per-tool activity, subagents (the `Task` tool tree, each child as its own nested row), plan approvals, and permission prompts. `SessionEnd` tombstones the row the moment the session closes.
- **Live context:** Claude's statusline is wrapped to push rich per-session data (context window, usage, cost, model display name) on its own cadence, so the card's context meter and dollar figure stay live rather than turn-grained. The context window is read from the model id, and a `[1m]` capability tag widens it.
- **Resume:** `claude --resume`; `rimz agents claude --resume` reopens the freshest closed session.
- **Permission modes:** `claude-{auto,ask,plan,yolo,ping}` as launch cells; on the command line `--ask` keeps Claude's own prompts and `--yolo` passes `--dangerously-skip-permissions`. Effort levels: `low|medium|high|xhigh|max`.
- **Account:** probed with `claude auth status` for plan and login state, feeding the provider dashboard.

Mapping detail: [claude.md](../internals/agents/claude.md); upstream protocol: [claude-reference.md](../externals/agent-adapter/claude-reference.md).

### Codex

Codex is a full integration with one structural difference: since 0.137 its hooks are **daemon-routed**, firing from a shared per-user app-server rather than the pane. Rimz recovers the pane from the in-pane `codex` process at the same working directory, so an in-pane Codex session still renders as a normal, jump-able row ([hooks resolve the room they live in](../internals/agents/model.md#hooks-resolve-the-room-they-live-in)).

- **What it reports:** turn boundaries, per-tool activity, subagents (thread-spawned children since 0.134), plan approvals, permission prompts, and the `notify` channel. Codex has no per-session end hook, so a closed session leaves by pane liveness and the rollup reaper rather than an explicit tombstone.
- **Live context:** the rollout `.jsonl` tail is the native live source for tokens, cost, and effort, read under a stat gate so an unchanged file costs nothing; the read-only app-server methods supply account and rate-limit context. The context window comes from the rollout's `model_context_window`.
- **Turn-death handling:** Codex can end a turn on a provider limit with no error record and no `Stop` hook. Rimz confirms these from a bounded pane capture plus the account budget, so a paused Codex reads as `⏸` rather than a false success or a stall ([turn-completion and turn-death markers](../internals/agents/codex.md#turn-completion-marker)); `rimz agents refresh @codex` re-runs the check on demand.
- **Resume:** `codex resume`; `rimz agents codex --resume` reopens the freshest closed session.
- **Permission modes:** `codex-{auto,ask,plan,yolo,ping}` as launch cells (`codex-plan` keeps the default posture, as Codex has no distinct plan mode); on the command line `--yolo` passes `--dangerously-bypass-approvals-and-sandbox`. Effort levels: `minimal|low|medium|high|xhigh`.

Mapping detail: [codex.md](../internals/agents/codex.md); upstream protocol: [codex-reference.md](../externals/agent-adapter/codex-reference.md).

### Pi

Pi runs in-process in its pane and reports through its **extension API**, which stamps the context gauge directly onto each hook envelope — so Pi needs no transcript tail or separate transport for live context.

- **What it reports:** turn boundaries, per-tool activity, and context usage on every envelope. Live cost is reconstructed from the session store at turn end, so the dashboard marks it partial coverage (`live$`) rather than provider-pushed.
- **Blocking asks:** Pi has no native prompt UI, so its adapter declares `native_ask_ui` off. A blocking hook returns the neutral no-op — which for Pi *is* the allow — and Rimz records no waiting row, because there is no native prompt a `?` row could route you to. Verify this per agent; Pi's neutral semantics differ from Claude's and Codex's.
- **Resume:** `pi --session`.
- **Permission modes:** `pi-{ask,plan}` as launch cells. Effort levels: `off|minimal|low|medium|high|xhigh`.
- **Account:** read from `~/.pi/agent/auth.json` — OAuth is a metered subscription, an API key is unmetered.

Mapping detail: [pi.md](../internals/agents/pi.md); upstream protocol: [pi-reference.md](../externals/agent-adapter/pi-reference.md).

### OpenCode

OpenCode reports through a **plugin** that maintains its context gauge from the agent's `message.updated` events and stamps the latest usage split, plus the model's context window from OpenCode's own catalog, onto each lifecycle envelope. Interactive OpenCode runs in-process in its pane and binds standalone.

- **What it reports:** turn boundaries, per-tool activity, and context usage per envelope. Session end and idle have no native hook, so both are reconstructed from pane liveness, the reaper, and turn boundaries; live cost is summed from the session store (SQLite) at turn end and marked partial (`live$`).
- **Resume:** the session `.jsonl` carries the history; see [opencode.md](../internals/agents/opencode.md) for the current resume path.
- **Permission modes:** no launch-cell suffixes yet — launch OpenCode bare or through a profile (`rimz agents --help` lists the current flags).
- **Account:** read from OpenCode's own `auth.json` — the probe reports the active provider, with OAuth as a metered subscription and an API key as unmetered. OpenCode exposes no plan tier or quota surface of its own, so usage bars come from the backing provider's endpoints, the same Anthropic and ChatGPT probes Claude and Codex use.

Mapping detail: [opencode.md](../internals/agents/opencode.md); upstream protocol: [opencode-reference.md](../externals/agent-adapter/opencode-reference.md).

## Versions

Rimz tracks each agent's own release surface, and behavior can shift with the agent's version — Codex, for example, moved to daemon-routed hooks at 0.137 and adjusted turn-completion signals through the 0.14x line. Rimz adapts to these at runtime rather than pinning a hard floor in this page, and `rimz doctor` reports any version drift it detects per agent after an upgrade ([troubleshooting](../guide/troubleshooting.md)). For the exact event surface a given agent version exposes, the authority is that agent's [adapter doc](../internals/agents/model.md) and [external reference](../externals/agent-adapter/claude-reference.md).

## How adapters work

Adding an agent is implementing one trait plus a descriptor and a single registry line; nothing else in Rimz changes, because status, ranking, liveness, cost, and blocking-ask routing all flow from the agent-agnostic observation the adapter emits. Two invariants keep the seam honest: adapters never write the store (they only classify and normalize), and nothing downstream ever reads a native payload. The full boundary, the two hook channels, and the install mechanics are in [the agent model](../internals/agents/model.md#the-adapter-boundary).

Installing an agent's hooks edits that agent's own config, so it is a visible, consented step: `rimz start` offers it on first run with a diff preview, `rimz hooks install --dry-run` prints the patch, and `rimz hooks uninstall` reverses it exactly ([hooks and trust reference](./cli/hooks-trust.md)). An agent run before its hooks are installed simply reports nothing — it is invisible, never silently broken.

## Agents not yet supported

An agent Rimz doesn't recognize runs fine in a pane; it renders as a plain process row rather than an agent card, with no live state or attention routing. New agents such as Cursor, Gemini, or Copilot land the same way the four here did — one adapter over their verified hook surface ([adding an agent](../internals/agents/model.md#adding-an-agent)). Two other categories are known gaps: **remote agents** with no local pane (a `claude remote-control --spawn` worktree, or a Codex thread started from the web) are tracked but not yet rendered, and an agent whose hooks you declined at the consent gate reports nothing until you wire it with `rimz hooks install`.

## See also

- [Agents](../guide/agents.md) — launching agents and profiles across every supported kind.
- [Teams](../guide/teams.md) — pairing models by role across supported kinds.
- [Messaging](../guide/messaging.md) — steering and queuing agents by handle.
- [The agent model](../internals/agents/model.md) — the rollup, state machine, and adapter boundary in depth.
- [Configuration](../guide/configuration.md#agent-profiles-commands-and-teams) — profiles, effort, and per-agent launch args.
- [Troubleshooting](../guide/troubleshooting.md) — `rimz doctor`, hooks not reporting, and version drift.
