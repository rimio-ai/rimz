# Agent support

RimZ watches the coding agents you already run — Claude Code, Codex, and the alpha and experimental set (Pi, OpenCode, Antigravity, Copilot, Droid, Cursor, Amp, Kiro, Qwen Code, Kimi, and Grok Build) — through one uniform adapter each. An adapter translates that agent's own hooks, transcripts, and APIs into the vocabulary the rest of RimZ speaks, so `rimz agents` launches, `rimz message` steers, and `rimz agents … -p` scripts every built-in that exposes it, all through the same boundary. It reads what the agent does and classifies it; you answer in the agent's own UI, the CLI runs stock, and the official web, desktop, and mobile apps keep working untouched. The boundary in depth is [the agent model](../internals/agents/model.md).

Read the support level honestly. **Claude Code and Codex are the supported daily drivers** — wired end to end and run constantly. **Pi and OpenCode are alpha**, close behind the daily drivers; the remaining agents are experimental, wired against their documented hook and transcript surface and covered by tests but not yet dogfooded enough by the author. Treat alpha and experimental integrations as best-effort: run them anyway, since they mostly just work, and please report the bugs you hit. Support tier tracks lived confidence, not mechanical breadth — an early-tier agent can still wire up a wide surface, as the matrix below shows.

Every integration is declared cell by cell, not assumed. Each adapter states its own coverage, conformance tests cross-check that declaration against the code that backs it, and `rimz coverage` prints the same matrices this page annotates — so what RimZ claims to read is a thing you verify on your own machine rather than take on faith:

Qwen's experimental Alibaba Coding Plan integration also enforces an exact managed-launch boundary outside the coverage score. Fresh supervised and loop launches bind the final official region and API key to an opaque account fingerprint, then use only that account's authoritative cached windows for exhaustion, priming, surplus, and reset recurrence. Exhaustion stops before a pane or loop check runs; interactive launches, resume, fork, wake, and mid-run auto-continue remain outside that boundary.

```sh
rimz coverage          # the wired / partial / unsupported grid, per agent, with a reason on every cell
rimz coverage --json   # the same, machine-readable
```

## The coverage matrix

`rimz coverage` scores each agent against sixteen product concerns. A cell reads **wired** (✓, native signals carry the full concern), **partial** (◐, native coverage is incomplete and RimZ reconstructs the rest from another signal or state), or **unsupported** (✗, unreachable from the agent's current protocol). A partial cell still shows you a live figure, and the command names the exact gap that derivation leaves.

One row per agent, ordered by support tier — Claude and Codex, then the alpha and experimental sets; `rimz coverage` prints them in registry order.

| Agent | `turn` | `perm` | `plan` | `ask` | `answer` | `compact` | `sub` | `bg` | `end` | `idle` | `usage` | `live$` | `rich` | `install` | `spend` | `remote` |
| --- | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: |
| Claude | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Codex | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✗ | ◐ | ◐ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Pi | ✓ | ✗ | ✗ | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ | ◐ | ✓ | ✓ | ✓ | ✓ | ✓ | ✗ |
| OpenCode | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ | ✓ | ✗ | ◐ | ◐ | ✓ | ◐ | ✓ | ✓ | ✓ | ✗ |
| Antigravity | ✓ | ◐ | ✗ | ◐ | ✗ | ✗ | ◐ | ✓ | ◐ | ◐ | ✓ | ◐ | ✓ | ✓ | ✗ | ✗ |
| Copilot | ✓ | ✓ | ✗ | ✓ | ✗ | ◐ | ◐ | ✗ | ✓ | ◐ | ✓ | ◐ | ✓ | ✓ | ✗ | ✗ |
| Droid | ✓ | ✗ | ✗ | ◐ | ✗ | ✓ | ✗ | ✗ | ✓ | ✓ | ✓ | ◐ | ◐ | ✓ | ✗ | ✗ |
| Cursor | ✓ | ✗ | ◐ | ◐ | ✗ | ◐ | ◐ | ✗ | ✓ | ◐ | ✓ | ◐ | ✓ | ✓ | ✗ | ✗ |
| Amp | ✓ | ✓ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ◐ | ◐ | ◐ | ◐ | ✗ | ✓ | ◐ | ✗ |
| Kiro | ◐ | ◐ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ◐ | ◐ | ◐ | ✗ | ✗ | ✗ | ✗ | ✗ |
| Qwen | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ◐ | ✓ | ✓ | ◐ | ✗ |
| Kimi | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ | ◐ | ✗ | ✓ | ◐ | ◐ | ✓ | ✗ | ✓ | ✓ | ✗ |
| Grok | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ | ✓ | ✗ | ✓ | ◐ | ✓ | ◐ | ◐ | ✓ | ✓ | ✗ |

<sub>✓ wired · ◐ partial (derived) · ✗ unsupported. Run `rimz coverage` for the live grid with the exact reason printed on every ◐ and ✗ cell.</sub>

What each concern column drives: `turn` live status (session start and every turn boundary), `perm` permission prompts routed to your keyboard, `plan` a plan-approval gate raising a waiting row, `ask` the agent's ask-the-user tool raising a waiting row, `answer` structured answers driving supported native prompt actions, `compact` context compaction on the card, `sub` the subagent tree as nested rows, `bg` a turn parked on background work, `end` the card tombstoning when the session closes, `idle` an idle nudge when the agent goes quiet, `usage` context-window fill and token counts, `live$` the live dollar figure, `rich` provider extras (official model labels, account windows), `install` RimZ installing the reporting hooks, `spend` account spend for the [token-insight](../guide/insight.md) dashboard, and `remote` driving or spawning a session with no local pane.

Claude Code is the reference integration and carries every concern natively; each other agent exposes less of its internals to a local observer. How much a given agent exposes is independent of how much it has been dogfooded — some early-tier agents wire up a wide surface, and some higher-tier ones deliberately leave cells derived. A ✗ is an honest declared absence — the sidebar and `rimz doctor` read the same declaration, so a missing surface renders as a stated gap rather than a silent bug.

### Notes on the alpha and experimental set

The gaps you will actually feel, per agent. Each agent's [mapping doc](#per-agent-mappings) carries the full rationale.

- **Antigravity** keeps permissions and questions in its own UI: RimZ deliberately installs no permission hook, so an open prompt raises the waiting card and routes you to the pane, nothing more. Child hooks join to the parent's ordered `invoke_subagent` transcript records, which keeps asynchronous and nested children under the root card through completion; the join is partial because the parent-result record is verified in 1.1.2 but not documented as a stable wire contract. Live model and context ride a wrapped statusline, and the account's `5h`/`7d` bars come from a read-only local service. Every Antigravity error stop is terminal, so a supervised run does not survive a provider limit and auto-continue never arms. `rimz hooks install antigravity --dry-run` previews the changes before consent, and uninstall restores the prior statusline.
- **Copilot** reports the concrete auto-selected model, effort, occupied-window tokens, current-call composition, and normalized cumulative session tokens through its wrapped statusline; those totals produce an estimated live dollar figure at the same resolved model, while metadata-only OTel remains the fallback when the bridge is absent or replaced. Finalized shutdown history adds per-model tokens and locally estimated dollars to `rimz stats` and the provider dashboard without claiming an authoritative billing ledger. Singular and batched ask hooks raise Waiting, and every post-tool completion clears it immediately after the native answer. Standard child prompt/stop hooks join to the parent's transcript records by exact task-call ID, publishing the model at start and the exact total after completion; child tools and permissions remain unavailable. Its bounded account query adds the plan and named monthly `cr`/`cht` plus genuine `prm` bars without inventing 5h/7d windows; statusline `ai_used`, IDE completions, extra credits, authoritative account dollars, and remote control remain unsupported. Install previews and manages both the hook file and `$COPILOT_HOME/settings.json` and restores a prior user statusline on uninstall.
- **Cursor** exposes its official `AskQuestion` and plan-approval UIs without corresponding hooks. RimZ derives an open synchronous question or **Ready to build?** plan proposal from Cursor's version-pinned local state, raises a pane-only waiting card, and routes you to the existing pane; `rimz asks` stays empty and `rimz answer` remains unsupported because the native UI is the only safe answer surface. A later conversation message clears the plan wait, while dismissing it with Esc or `p` leaves the waiting card until the next turn because Cursor writes no store change. Subagents are partial: the pinned CLI defines and accepts `subagentStart`/`subagentStop` hooks but never issues those requests, so RimZ derives exact child/parent lifecycle from the chats store and child transcript when the next parent hook feeds, often only at turn end. Live context rides its statusline, and RimZ prices each generation locally; that running session total counts toward live agent and room [budgets](../guide/budget.md), while provider billing, account spend, and `rimz stats` stay unavailable. The install manages `~/.cursor/hooks.json` and the statusline in `~/.cursor/cli-config.json`, shows both diffs before consent, and restores the prior statusline on uninstall.
- **Droid** misses the same ask wire, but RimZ derives the waiting card from the transcript's active `AskUser` call; the answer still happens in Droid's pane. Its locally priced session total reaches the card and live budgets the way Cursor's does, while provider dollars, historical spend, and quota stay unavailable.
- **Kiro** did not execute its documented hooks under verification, so RimZ pulls its lifecycle from Kiro's local session store instead. A pending tool approval still marks the card waiting, but `rimz asks` and `rimz answer` do not claim it; context is percentage-only, and hook install and supervised `-p` runs are unsupported.
- **Kimi** joins `SubagentStart` and `SubagentStop` previews to validated child entries in `state.json` and their `wire.jsonl` records, namespacing Kimi's per-session child counters before creating nested rows. Resumed children and ambiguous concurrent starts expose no start-time identity and therefore appear only when the response-side Stop match becomes unique. Child-fired session `Stop` hooks are suppressed while the main wire is provably mid-step, so the parent stays running until its own final Stop.
- **Grok** installs only passive global hooks, normalizes Grok's snake_case event values before lifecycle dispatch, and leaves every permission decision in the native TUI. Its rewind-aware `updates.jsonl` fold supplies full history, child lifecycle, active context, and exact completed-turn dollars; live mid-turn cost and account quota windows remain unavailable. Ask uses `--permission-mode default`, Auto uses `--permission-mode auto`, Yolo uses `--yolo`, and Plan intentionally adds no argv because `/plan` is interactive rather than an enforced launch posture.

## The lifecycle hook surface

Under the concern matrix sits the raw event surface: the eleven lifecycle signals RimZ folds into every agent's state machine, and the native event each agent fires for each one. `rimz coverage` prints this as its second grid, the hooks matrix; here it is with the native event names in place, in the same support-tier order.

| Agent | `registered` | `turn_started` | `turn_ended` | `tool_used` | `awaiting_input` | `subagent_started` | `subagent_stopped` | `compacting` | `compaction_ended` | `ended` | `lost` |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Claude | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `PermissionRequest` | `SubagentStart` | `SubagentStop` | `PreCompact` | `PostCompact` | `SessionEnd` | ◐ derived |
| Codex | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `PermissionRequest`; `Stop` + rollout `Plan` | `SubagentStart` | `SubagentStop` | `PreCompact` | `PostCompact` | ◐ derived | ◐ derived |
| Pi | `session_start` | `before_agent_start` | `agent_settled` (`agent_end` before Pi 0.80.4) | `tool_execution_end` | `tool_call` | `subagent_started` | `subagent_stopped` | `session_before_compact` | `session_compact` | `session_shutdown` | ◐ derived |
| OpenCode | `session_created` | `chat_message` | `session_idle` | `tool_after` | `permission_ask`; `session_idle` + plan turn | `SubagentStart` | `SubagentStop` | `session_compacting` | `session_compacted` | ◐ derived | ◐ derived |
| Antigravity | ◐ first `PreInvocation` identity + local discovery | `PreInvocation` | `Stop` | `PostToolUse` | ◐ statusline permission marker + transcript question | ◐ child `PreInvocation` + parent transcript join | ◐ child `Stop` + parent transcript join | ✗ | ✗ | ◐ derived | ◐ derived |
| Copilot | `sessionStart` | `userPromptSubmitted` | `agentStop` | `postToolUse` | `permissionRequest` | ◐ child `userPromptSubmitted` + parent transcript join | ◐ child `agentStop` + parent transcript join | `preCompact` | ◐ derived | `sessionEnd` | ◐ derived |
| Droid | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | ◐ transcript `AskUser` | ✗ | ✗ | `PreCompact` | `SessionStart:compact` | `SessionEnd` | ◐ derived |
| Cursor | `sessionStart` | `beforeSubmitPrompt` | `stop` | `postToolUse` | ◐ local pending `AskQuestion` or plan proposal | `subagentStart` | `subagentStop` | `preCompact` | ◐ derived | `sessionEnd` | ◐ derived |
| Amp | `session_start` | `agent_start` | `agent_end` | `tool_result` | `permission_ask` | ✗ | ✗ | ✗ | ✗ | ◐ derived | ◐ derived |
| Kiro | ◐ local store | ◐ `turn_start` | ◐ `turn_end` | ◐ tool records | ◐ pending interaction | ✗ | ✗ | ✗ | ✗ | ◐ derived | ◐ derived |
| Qwen | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `PermissionRequest` | `SubagentStart` | `SubagentStop` | `PreCompact` | `PostCompact` | `SessionEnd` | ◐ derived |
| Kimi | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `PermissionRequest` | ◐ `SubagentStart` + durable child join | ◐ `SubagentStop` + durable child join | `PreCompact` | `PostCompact` | `SessionEnd` | ◐ derived |
| Grok | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `Notification` | `SubagentStart` | `SubagentStop` | `PreCompact` | `PostCompact` | `SessionEnd` | ◐ derived |

`lost` — an agent's mux-session dying out from under it — has no native event in any built-in, because an agent's own hooks stop firing exactly when the thing that would report the death is gone. RimZ derives it from the `rimz exec` launch wrapper instead. Where `ended` is derived (Codex, OpenCode, Antigravity, Amp, Kiro), the same pane-liveness-and-reaper path clears the row on the next snapshot tick rather than at the instant of exit.

## Per-agent mappings

The detail for each agent — its full coverage rationale, permission-mode mapping, effort levels, install target, resume/fork surface, and account probing — lives in that agent's mapping doc, with its upstream protocol in the matching external reference. Adding an agent is implementing one trait plus a descriptor and a single registry line ([adding an agent](../internals/agents/model.md#adding-an-agent)).

| Agent | Mapping | Upstream protocol |
| --- | --- | --- |
| Claude Code | [claude.md](../internals/agents/claude.md) | [claude-reference.md](../externals/agent-adapter/claude-reference.md) |
| Codex | [codex.md](../internals/agents/codex.md) | [codex-reference.md](../externals/agent-adapter/codex-reference.md) |
| Pi | [pi.md](../internals/agents/pi.md) | [pi-reference.md](../externals/agent-adapter/pi-reference.md) |
| OpenCode | [opencode.md](../internals/agents/opencode.md) | [opencode-reference.md](../externals/agent-adapter/opencode-reference.md) |
| Antigravity CLI | [antigravity.md](../internals/agents/antigravity.md) | [antigravity-reference.md](../externals/agent-adapter/antigravity-reference.md) |
| Copilot | [copilot.md](../internals/agents/copilot.md) | [copilot-reference.md](../externals/agent-adapter/copilot-reference.md) |
| Droid | [droid.md](../internals/agents/droid.md) | [droid-reference.md](../externals/agent-adapter/droid-reference.md) |
| Cursor | [cursor.md](../internals/agents/cursor.md) | [cursor-reference.md](../externals/agent-adapter/cursor-reference.md) |
| Amp | [amp.md](../internals/agents/amp.md) | [amp-reference.md](../externals/agent-adapter/amp-reference.md) |
| Kiro CLI | [kiro.md](../internals/agents/kiro.md) | [kiro-reference.md](../externals/agent-adapter/kiro-reference.md) |
| Qwen Code | [qwen.md](../internals/agents/qwen.md) | [qwen-reference.md](../externals/agent-adapter/qwen-reference.md) |
| Kimi | [kimi.md](../internals/agents/kimi.md) | [kimi-reference.md](../externals/agent-adapter/kimi-reference.md) |
| Grok Build | [grok.md](../internals/agents/grok.md) | [grok-reference.md](../externals/agent-adapter/grok-reference.md) |

## Versions

RimZ tracks each agent's own release surface, and behaviour can shift with the agent's version — Codex, for example, moved to daemon-routed hooks at 0.137 and adjusted turn-completion signals through the 0.14x line. RimZ adapts at runtime rather than pinning a hard floor here, and `rimz doctor` reports version drift it detects per agent after an upgrade ([troubleshooting](../guide/troubleshooting.md)). For the exact event surface a given agent version exposes, the authority is that agent's [mapping doc](#per-agent-mappings) and external reference.

## Agents not yet supported

An agent RimZ doesn't recognize runs fine in a pane; it renders as a plain process row rather than an agent card, with no live state or attention routing. New agents land the same way the built-ins here did — one adapter over their verified hook or local-store surface ([adding an agent](../internals/agents/model.md#adding-an-agent)). Two categories are known gaps: **remote agents** with no local pane (a `claude remote-control --spawn` worktree, or a Codex thread started from the web) are tracked but not yet rendered, and an agent whose hooks you declined at the consent gate reports nothing until you wire it with `rimz hooks install`.

## Third-party plugins

A machine-tier process-plugin path lets a third-party agent reach the same adapter boundary through a shim that speaks a canonical event protocol. It is under active development and not yet mature for outside use; feature status there is bundle-specific rather than a RimZ release tier. The in-progress contract is [agent plugins](./agent-plugins.md).

## See also

- [Agents](../guide/fleet.md) — launching agents and profiles across every supported kind.
- [Teams](../guide/teams.md) — pairing models by role across supported kinds.
- [Messaging](../guide/messaging.md) — steering and queuing agents by handle.
- [Token insight](../guide/insight.md) — where the `live$` and `spend` figures surface, and how each is calculated.
- [The agent model](../internals/agents/model.md) — the rollup, state machine, and adapter boundary in depth.
- [Configuration](../guide/configuration.md#agent-profiles-commands-and-teams) — profiles, effort, and per-agent launch args.
- [Troubleshooting](../guide/troubleshooting.md) — `rimz doctor`, hooks not reporting, and version drift.
