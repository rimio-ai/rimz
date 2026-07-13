# Agent support

RimZ watches the coding agents you already run — Claude Code, Codex, Pi, OpenCode, and the experimental set (Gemini CLI, Copilot, Droid, Cursor, Amp, Kiro, Qwen Code, and Kimi) — through one uniform adapter each. An adapter translates that agent's own hooks, transcripts, and APIs into the vocabulary the rest of RimZ speaks, so `rimz agents` launches, `rimz message` steers, and `rimz agents … -p` scripts every built-in that exposes it, all through the same boundary. It reads what the agent does and classifies it; you answer in the agent's own UI, the CLI runs stock, and the official web, desktop, and mobile apps keep working untouched. The boundary in depth is [the agent model](../internals/agents/model.md).

Read the support level honestly. **Claude Code and Codex are the supported daily drivers** — wired end to end and run constantly. **Pi is beta** and **OpenCode is alpha**: used regularly and complete enough to trust, with a rougher edge on enrichment. **Every other agent is experimental** — wired against its documented hook and transcript surface and covered by tests, but not yet dogfooded by the author, who holds a paid subscription for only a few of them. Treat those as best-effort: run them anyway, since they mostly just work, and please report the bugs you hit. Support tier tracks that lived confidence, not mechanical breadth — an experimental agent can still wire up a wide surface, as the matrix below shows.

Every integration is declared cell by cell, not assumed. Each adapter states its own coverage, conformance tests cross-check that declaration against the code that backs it, and `rimz coverage` prints the same matrices this page annotates — so what RimZ claims to read is a thing you verify on your own machine rather than take on faith:

```sh
rimz coverage          # the wired / partial / unsupported grid, per agent, with a reason on every cell
rimz coverage --json   # the same, machine-readable
```

## The coverage matrix

`rimz coverage` scores each agent against sixteen product concerns. A cell reads **wired** (✓, native signals carry the full concern), **partial** (◐, native coverage is incomplete and RimZ reconstructs the rest from another signal or state), or **unsupported** (✗, unreachable from the agent's current protocol). A partial cell still shows you a live figure, and the command names the exact gap that derivation leaves.

One row per agent, ordered by support tier — Claude, Codex, Pi, OpenCode, then the experimental set; `rimz coverage` prints them in registry order.

| Agent | `turn` | `perm` | `plan` | `ask` | `answer` | `compact` | `sub` | `bg` | `end` | `idle` | `usage` | `live$` | `rich` | `install` | `spend` | `remote` |
| --- | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: |
| Claude | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Codex | ✓ | ✓ | ✗ | ✓ | ✗ | ✓ | ✓ | ✗ | ◐ | ◐ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Pi | ✓ | ✓ | ✗ | ✗ | ✗ | ✓ | ✗ | ✗ | ✓ | ◐ | ✓ | ◐ | ◐ | ✓ | ✓ | ✗ |
| OpenCode | ✓ | ✓ | ✗ | ✓ | ✗ | ✓ | ✓ | ✗ | ◐ | ◐ | ✓ | ◐ | ✓ | ✓ | ✓ | ✗ |
| Gemini | ✓ | ✓ | ✓ | ✓ | ✗ | ◐ | ✗ | ✗ | ✓ | ◐ | ✓ | ◐ | ✗ | ✓ | ◐ | ✗ |
| Copilot | ✓ | ✓ | ✗ | ✓ | ✗ | ◐ | ✗ | ✗ | ✓ | ◐ | ◐ | ✗ | ◐ | ✓ | ✗ | ✗ |
| Droid | ✓ | ✗ | ✗ | ✗ | ✗ | ✓ | ✗ | ✗ | ✓ | ✓ | ✗ | ✗ | ✗ | ✓ | ✗ | ✗ |
| Cursor | ✓ | ✗ | ✗ | ✗ | ✗ | ◐ | ✗ | ✗ | ✓ | ◐ | ✓ | ✗ | ✗ | ✓ | ✗ | ✗ |
| Amp | ✓ | ✓ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ◐ | ◐ | ◐ | ◐ | ✗ | ✓ | ◐ | ✗ |
| Kiro | ✓ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ◐ | ◐ | ✗ | ✗ | ✗ | ✓ | ✗ | ✗ |
| Qwen | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ◐ | ✓ | ✓ | ◐ | ✗ |
| Kimi | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ | ◐ | ✗ | ✓ | ◐ | ◐ | ✓ | ✗ | ✓ | ✓ | ✗ |

<sub>✓ wired · ◐ partial (derived) · ✗ unsupported. Run `rimz coverage` for the live grid with the exact reason printed on every ◐ and ✗ cell.</sub>

What each concern column drives: `turn` live status (session start and every turn boundary), `perm` permission prompts routed to your keyboard, `plan` a plan-approval gate raising a waiting row, `ask` the agent's ask-the-user tool raising a waiting row, `answer` structured answers driving supported native prompt actions, `compact` context compaction on the card, `sub` the subagent tree as nested rows, `bg` a turn parked on background work, `end` the card tombstoning when the session closes, `idle` an idle nudge when the agent goes quiet, `usage` context-window fill and token counts, `live$` the live dollar figure, `rich` provider extras (official model labels, account windows), `install` RimZ installing the reporting hooks, `spend` account spend for the [token-insight](../guide/insight.md) dashboard, and `remote` driving or spawning a session with no local pane.

Claude Code is the reference integration and carries every concern natively; each other agent exposes less of its internals to a local observer. How much a given agent exposes is independent of how much it has been dogfooded — some experimental agents wire up a wide surface, and some higher-tier ones deliberately leave cells derived. A ✗ is an honest declared absence — the sidebar and `rimz doctor` read the same declaration, so a missing surface renders as a stated gap rather than a silent bug.

Copilot history and supervised final output read its per-session `events.jsonl`. Its partial live usage and rich context come from opt-in, metadata-only OTel `chat` spans: the card can show the resolved model and latest-call token composition, but Copilot publishes no authoritative context-window denominator, quota, account metadata, or session-dollar total through that narrow source.

## The lifecycle hook surface

Under the concern matrix sits the raw event surface: the eleven lifecycle signals RimZ folds into every agent's state machine, and the native event each agent fires for each one. `rimz coverage` prints this as its second grid, the hooks matrix; here it is with the native event names in place, in the same support-tier order.

| Agent | `registered` | `turn_started` | `turn_ended` | `tool_used` | `awaiting_input` | `subagent_started` | `subagent_stopped` | `compacting` | `compaction_ended` | `ended` | `lost` |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Claude | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `PermissionRequest` | `SubagentStart` | `SubagentStop` | `PreCompact` | `PostCompact` | `SessionEnd` | ◐ derived |
| Codex | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `PermissionRequest` | `SubagentStart` | `SubagentStop` | `PreCompact` | `PostCompact` | ◐ derived | ◐ derived |
| Pi | `session_start` | `before_agent_start` | `agent_settled` (`agent_end` before Pi 0.80.4) | `tool_execution_end` | ✗ | ✗ | ✗ | `session_before_compact` | `session_compact` | `session_shutdown` | ◐ derived |
| OpenCode | `session_created` | `chat_message` | `session_idle` | `tool_after` | `permission_ask` | `SubagentStart` | `SubagentStop` | `session_compacting` | `session_compacted` | ◐ derived | ◐ derived |
| Gemini | `SessionStart` | `BeforeAgent` | `AfterAgent` | `AfterTool` | `Notification` | ✗ | ✗ | `PreCompress` | ◐ derived | `SessionEnd` | ◐ derived |
| Copilot | `sessionStart` | `userPromptSubmitted` | `agentStop` | `postToolUse` | `permissionRequest` | ✗ | ✗ | `preCompact` | ◐ derived | `sessionEnd` | ◐ derived |
| Droid | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | ✗ | ✗ | ✗ | `PreCompact` | `SessionStart:compact` | `SessionEnd` | ◐ derived |
| Cursor | `sessionStart` | `beforeSubmitPrompt` | `stop` | `postToolUse` | ✗ | ✗ | ✗ | `preCompact` | ◐ derived | `sessionEnd` | ◐ derived |
| Amp | `session_start` | `agent_start` | `agent_end` | `tool_result` | `permission_ask` | ✗ | ✗ | ✗ | ✗ | ◐ derived | ◐ derived |
| Kiro | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | ✗ | ✗ | ✗ | ✗ | ✗ | ◐ derived | ◐ derived |
| Qwen | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `PermissionRequest` | `SubagentStart` | `SubagentStop` | `PreCompact` | `PostCompact` | `SessionEnd` | ◐ derived |
| Kimi | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `PermissionRequest` | `SubagentStart` | `SubagentStop` | `PreCompact` | `PostCompact` | `SessionEnd` | ◐ derived |

`lost` — an agent's mux-session dying out from under it — has no native event in any built-in, because an agent's own hooks stop firing exactly when the thing that would report the death is gone. RimZ derives it from the `rimz exec` launch wrapper instead. Where `ended` is derived (Codex, OpenCode, Amp, Kiro), the same pane-liveness-and-reaper path clears the row on the next snapshot tick rather than at the instant of exit.

## Per-agent mappings

The detail for each agent — its full coverage rationale, permission-mode mapping, effort levels, install target, resume/fork surface, and account probing — lives in that agent's mapping doc, with its upstream protocol in the matching external reference. Adding an agent is implementing one trait plus a descriptor and a single registry line ([adding an agent](../internals/agents/model.md#adding-an-agent)).

| Agent | Mapping | Upstream protocol |
| --- | --- | --- |
| Claude Code | [claude.md](../internals/agents/claude.md) | [claude-reference.md](../externals/agent-adapter/claude-reference.md) |
| Codex | [codex.md](../internals/agents/codex.md) | [codex-reference.md](../externals/agent-adapter/codex-reference.md) |
| Pi | [pi.md](../internals/agents/pi.md) | [pi-reference.md](../externals/agent-adapter/pi-reference.md) |
| OpenCode | [opencode.md](../internals/agents/opencode.md) | [opencode-reference.md](../externals/agent-adapter/opencode-reference.md) |
| Gemini CLI | [gemini.md](../internals/agents/gemini.md) | [gemini-reference.md](../externals/agent-adapter/gemini-reference.md) |
| Copilot | [copilot.md](../internals/agents/copilot.md) | [copilot-reference.md](../externals/agent-adapter/copilot-reference.md) |
| Droid | [droid.md](../internals/agents/droid.md) | [droid-reference.md](../externals/agent-adapter/droid-reference.md) |
| Cursor | [cursor.md](../internals/agents/cursor.md) | [cursor-reference.md](../externals/agent-adapter/cursor-reference.md) |
| Amp | [amp.md](../internals/agents/amp.md) | [amp-reference.md](../externals/agent-adapter/amp-reference.md) |
| Kiro CLI | [kiro.md](../internals/agents/kiro.md) | [kiro-reference.md](../externals/agent-adapter/kiro-reference.md) |
| Qwen Code | [qwen.md](../internals/agents/qwen.md) | [qwen-reference.md](../externals/agent-adapter/qwen-reference.md) |
| Kimi | [kimi.md](../internals/agents/kimi.md) | [kimi-reference.md](../externals/agent-adapter/kimi-reference.md) |

Two agents carry a deliberate absence worth stating here: Cursor and Droid appear and report their work, but their stock local hooks cannot signal that a native question is open, so they have no waiting row or ask routing — answer those prompts in the agent's own pane.

## Versions

RimZ tracks each agent's own release surface, and behaviour can shift with the agent's version — Codex, for example, moved to daemon-routed hooks at 0.137 and adjusted turn-completion signals through the 0.14x line. RimZ adapts at runtime rather than pinning a hard floor here, and `rimz doctor` reports version drift it detects per agent after an upgrade ([troubleshooting](../guide/troubleshooting.md)). For the exact event surface a given agent version exposes, the authority is that agent's [mapping doc](#per-agent-mappings) and external reference.

## Agents not yet supported

An agent RimZ doesn't recognize runs fine in a pane; it renders as a plain process row rather than an agent card, with no live state or attention routing. New agents land the same way the built-ins here did — one adapter over their verified hook surface ([adding an agent](../internals/agents/model.md#adding-an-agent)). Two categories are known gaps: **remote agents** with no local pane (a `claude remote-control --spawn` worktree, or a Codex thread started from the web) are tracked but not yet rendered, and an agent whose hooks you declined at the consent gate reports nothing until you wire it with `rimz hooks install`.

## Third-party plugins

A machine-tier process-plugin path lets a third-party agent reach the same adapter boundary through a shim that speaks a canonical event protocol. It is under active development and not yet mature for outside use; feature status there is bundle-specific rather than a RimZ release tier. The in-progress contract is [agent plugins](./agent-plugins.md).

## See also

- [Agents](../guide/agents.md) — launching agents and profiles across every supported kind.
- [Teams](../guide/teams.md) — pairing models by role across supported kinds.
- [Messaging](../guide/messaging.md) — steering and queuing agents by handle.
- [Token insight](../guide/insight.md) — where the `live$` and `spend` figures surface, and how each is calculated.
- [The agent model](../internals/agents/model.md) — the rollup, state machine, and adapter boundary in depth.
- [Configuration](../guide/configuration.md#agent-profiles-commands-and-teams) — profiles, effort, and per-agent launch args.
- [Troubleshooting](../guide/troubleshooting.md) — `rimz doctor`, hooks not reporting, and version drift.
