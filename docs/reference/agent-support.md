# Agent support

RimZ watches the coding agents you already run — Claude Code, Codex, and the experimental set (Pi, OpenCode, Antigravity, Copilot, Droid, Cursor, Amp, Kiro, Qwen Code, and Kimi) — through one uniform adapter each. An adapter translates that agent's own hooks, transcripts, and APIs into the vocabulary the rest of RimZ speaks, so `rimz agents` launches, `rimz message` steers, and `rimz agents … -p` scripts every built-in that exposes it, all through the same boundary. It reads what the agent does and classifies it; you answer in the agent's own UI, the CLI runs stock, and the official web, desktop, and mobile apps keep working untouched. The boundary in depth is [the agent model](../internals/agents/model.md).

Read the support level honestly. **Claude Code and Codex are the supported daily drivers** — wired end to end and run constantly. **Every other agent is experimental** — wired against its documented hook and transcript surface and covered by tests, but not yet dogfooded by the author, who holds a paid subscription for only a few of them. Treat those as best-effort: run them anyway, since they mostly just work, and please report the bugs you hit. Support tier tracks that lived confidence, not mechanical breadth — an experimental agent can still wire up a wide surface, as the matrix below shows.

Every integration is declared cell by cell, not assumed. Each adapter states its own coverage, conformance tests cross-check that declaration against the code that backs it, and `rimz coverage` prints the same matrices this page annotates — so what RimZ claims to read is a thing you verify on your own machine rather than take on faith:

```sh
rimz coverage          # the wired / partial / unsupported grid, per agent, with a reason on every cell
rimz coverage --json   # the same, machine-readable
```

## The coverage matrix

`rimz coverage` scores each agent against sixteen product concerns. A cell reads **wired** (✓, native signals carry the full concern), **partial** (◐, native coverage is incomplete and RimZ reconstructs the rest from another signal or state), or **unsupported** (✗, unreachable from the agent's current protocol). A partial cell still shows you a live figure, and the command names the exact gap that derivation leaves.

One row per agent, ordered by support tier — Claude and Codex, then the experimental set; `rimz coverage` prints them in registry order.

| Agent | `turn` | `perm` | `plan` | `ask` | `answer` | `compact` | `sub` | `bg` | `end` | `idle` | `usage` | `live$` | `rich` | `install` | `spend` | `remote` |
| --- | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: |
| Claude | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Codex | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✗ | ◐ | ◐ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Pi | ✓ | ✓ | ✗ | ✗ | ✗ | ✓ | ✗ | ✗ | ✓ | ◐ | ✓ | ◐ | ◐ | ✓ | ✓ | ✗ |
| OpenCode | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ | ✓ | ✗ | ◐ | ◐ | ✓ | ◐ | ✓ | ✓ | ✓ | ✗ |
| Antigravity | ✓ | ◐ | ✗ | ✗ | ✗ | ✗ | ✗ | ✓ | ◐ | ◐ | ✓ | ✗ | ✓ | ✓ | ✗ | ✗ |
| Copilot | ✓ | ✓ | ✗ | ✓ | ✗ | ◐ | ✗ | ✗ | ✓ | ◐ | ◐ | ✗ | ◐ | ✓ | ✗ | ✗ |
| Droid | ✓ | ✗ | ✗ | ◐ | ✗ | ✓ | ✗ | ✗ | ✓ | ✓ | ✓ | ◐ | ◐ | ✓ | ✗ | ✗ |
| Cursor | ✓ | ✗ | ✗ | ✗ | ✗ | ◐ | ✗ | ✗ | ✓ | ◐ | ✓ | ◐ | ✓ | ✓ | ✗ | ✗ |
| Amp | ✓ | ✓ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ◐ | ◐ | ◐ | ◐ | ✗ | ✓ | ◐ | ✗ |
| Kiro | ◐ | ◐ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ◐ | ◐ | ◐ | ✗ | ✗ | ✗ | ✗ | ✗ |
| Qwen | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ◐ | ✓ | ✓ | ◐ | ✗ |
| Kimi | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ | ◐ | ✗ | ✓ | ◐ | ◐ | ✓ | ✗ | ✓ | ✓ | ✗ |

<sub>✓ wired · ◐ partial (derived) · ✗ unsupported. Run `rimz coverage` for the live grid with the exact reason printed on every ◐ and ✗ cell.</sub>

Droid conversation history is partial: version-2 visible turns and final assistant output are available. Its `usage` cell reads 0.171.0's current-call composition plus cumulative token categories from the sibling session settings snapshot, while the partial `ask` cell projects an active transcript `AskUser` call to a native waiting card. Effort, custom-model identity, known capacity, and an exact-table local session price also reach the card and live cockpit and agent/room budgets; durable asks and answers, authoritative provider USD, historical or account spend, and quota remain unavailable.

What each concern column drives: `turn` live status (session start and every turn boundary), `perm` permission prompts routed to your keyboard, `plan` a plan-approval gate raising a waiting row, `ask` the agent's ask-the-user tool raising a waiting row, `answer` structured answers driving supported native prompt actions, `compact` context compaction on the card, `sub` the subagent tree as nested rows, `bg` a turn parked on background work, `end` the card tombstoning when the session closes, `idle` an idle nudge when the agent goes quiet, `usage` context-window fill and token counts, `live$` the live dollar figure, `rich` provider extras (official model labels, account windows), `install` RimZ installing the reporting hooks, `spend` account spend for the [token-insight](../guide/insight.md) dashboard, and `remote` driving or spawning a session with no local pane.

Claude Code is the reference integration and carries every concern natively; each other agent exposes less of its internals to a local observer. How much a given agent exposes is independent of how much it has been dogfooded — some experimental agents wire up a wide surface, and some higher-tier ones deliberately leave cells derived. A ✗ is an honest declared absence — the sidebar and `rimz doctor` read the same declaration, so a missing surface renders as a stated gap rather than a silent bug.

Copilot history and supervised final output read its per-session `events.jsonl`. New RimZ rooms enable a private, metadata-only OTel file for direct and managed Copilot launches: the card shows the resolved model and latest-call token composition, but the verified source publishes no authoritative context-window denominator or session-dollar total. An explicit file exporter is preserved, and an OTLP-only configuration leaves this enrichment unavailable. The idle account probe reads only host-safe non-secret identity from local `config.json` and presents the validated login as metered with an unknown subscription allowance. Environment tokens do not establish an account; plan, quota, extra credits, API-key balance, realtime cost, historical/account spend, browser billing, keychains, and plaintext `copilotTokens` stay unsupported.

Kiro history and live state are pulled from the stock v3 `session.json`/`messages.jsonl` store. A validated newborn session binds as an idle card before the first prompt when its cwd and pane process incarnation identify it safely. An unresolved native tool approval marks the card waiting, but Kiro has no RimZ-installed hook or mapped prompt choreography, so `rimz asks` and `rimz answer` do not claim that interaction. Context is percentage-only; tokens, USD, and credits remain unprojected.

Antigravity 1.1.2 combines safe native `PreInvocation`, `PostToolUse`, and `Stop` hooks with the stock workspace conversation cache and `brain/<conversation-id>/.system_generated/logs/transcript.jsonl`. Its wrapped custom statusline supplies model, version, plan/account identity, context usage, and a read-only permission-wait marker that raises the card and routes focus to the pane. A private read-only service on a verified same-uid running `agy` process supplies idle account identity and conservatively normalized authoritative `5h`/`7d` quota bars; RimZ starts no helper, reads no credential, and claims no credits or account dollars. Hook-bound sessions and exact `agy --conversation <id>` resumes bind independently of the latest cache, so a lagging workspace entry cannot hide a transcript-backed native question. RimZ leaves `PreToolUse` uninstalled because every documented response changes native permission policy, so the permission decision and all question/artifact waits remain in Antigravity's own pane.

## The lifecycle hook surface

Under the concern matrix sits the raw event surface: the eleven lifecycle signals RimZ folds into every agent's state machine, and the native event each agent fires for each one. `rimz coverage` prints this as its second grid, the hooks matrix; here it is with the native event names in place, in the same support-tier order.

| Agent | `registered` | `turn_started` | `turn_ended` | `tool_used` | `awaiting_input` | `subagent_started` | `subagent_stopped` | `compacting` | `compaction_ended` | `ended` | `lost` |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Claude | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `PermissionRequest` | `SubagentStart` | `SubagentStop` | `PreCompact` | `PostCompact` | `SessionEnd` | ◐ derived |
| Codex | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `PermissionRequest`; `Stop` + rollout `Plan` | `SubagentStart` | `SubagentStop` | `PreCompact` | `PostCompact` | ◐ derived | ◐ derived |
| Pi | `session_start` | `before_agent_start` | `agent_settled` (`agent_end` before Pi 0.80.4) | `tool_execution_end` | ✗ | ✗ | ✗ | `session_before_compact` | `session_compact` | `session_shutdown` | ◐ derived |
| OpenCode | `session_created` | `chat_message` | `session_idle` | `tool_after` | `permission_ask`; `session_idle` + plan turn | `SubagentStart` | `SubagentStop` | `session_compacting` | `session_compacted` | ◐ derived | ◐ derived |
| Antigravity | ◐ first `PreInvocation` identity + local discovery | `PreInvocation` | `Stop` | `PostToolUse` | ◐ statusline permission marker + transcript question | ✗ | ✗ | ✗ | ✗ | ◐ derived | ◐ derived |
| Copilot | `sessionStart` | `userPromptSubmitted` | `agentStop` | `postToolUse` | `permissionRequest` | ✗ | ✗ | `preCompact` | ◐ derived | `sessionEnd` | ◐ derived |
| Droid | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | ◐ transcript `AskUser` | ✗ | ✗ | `PreCompact` | `SessionStart:compact` | `SessionEnd` | ◐ derived |
| Cursor | `sessionStart` | `beforeSubmitPrompt` | `stop` | `postToolUse` | ✗ | ✗ | ✗ | `preCompact` | ◐ derived | `sessionEnd` | ◐ derived |
| Amp | `session_start` | `agent_start` | `agent_end` | `tool_result` | `permission_ask` | ✗ | ✗ | ✗ | ✗ | ◐ derived | ◐ derived |
| Kiro | ◐ local store | ◐ `turn_start` | ◐ `turn_end` | ◐ tool records | ◐ pending interaction | ✗ | ✗ | ✗ | ✗ | ◐ derived | ◐ derived |
| Qwen | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `PermissionRequest` | `SubagentStart` | `SubagentStop` | `PreCompact` | `PostCompact` | `SessionEnd` | ◐ derived |
| Kimi | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `PermissionRequest` | `SubagentStart` | `SubagentStop` | `PreCompact` | `PostCompact` | `SessionEnd` | ◐ derived |

`lost` — an agent's mux-session dying out from under it — has no native event in any built-in, because an agent's own hooks stop firing exactly when the thing that would report the death is gone. RimZ derives it from the `rimz exec` launch wrapper instead. Where `ended` is derived (Codex, OpenCode, Antigravity, Amp, Kiro), the same pane-liveness-and-reaper path clears the row on the next snapshot tick rather than at the instant of exit.

Kiro CLI 2.12.1 v3 did not execute documented user or project standalone hook configs during authenticated stock-TUI verification. RimZ instead binds validated provider-owned local sessions to live Kiro panes and derives their display lifecycle from physical record order. Hook installation and supervised `-p` remain unsupported because pulled files are not an executable completion channel.

Antigravity CLI 1.1.2 documents observer-neutral `{}` output for invocation and post-tool hooks and a non-`continue` `Stop` decision that permits termination. RimZ installs those events and wraps the custom statusline, while deliberately excluding `PreToolUse`; the provider's UI remains the only permission and question decision surface. Validated transcript questions project native waiting cards, priceable statusline model/current-token values provide a local API-rate estimate for the live card without claiming historical or provider billing, and the verified running local service fills account and quota between turns without OAuth or a spawned `agy`. `rimz hooks install antigravity --dry-run` shows both `hooks.json` and `settings.json` changes before consent, and uninstall restores the prior statusline command.

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

Cursor carries a deliberate absence worth stating here: it appears and reports its work, but its stock local hooks cannot signal that a native question is open, so it has no waiting row or ask routing. Droid's hook wire has the same absence, but its validated 0.171.0 transcript now derives the waiting row from the active `AskUser` record. Answer either agent's prompt in its own pane; Droid's derived marker creates no structured RimZ answer surface.

Cursor's live context is statusline-backed: the card reads Cursor's display model, window, fill, version, and current token composition, while `preCompact` and `stop` provide fallbacks. The idle account probe reads only the resolved CLI's documented status/about JSON, publishing a reconciled email, raw tier, and CLI version without reading credentials or browser state; quota and paid usage remain unavailable. Response and stop hooks repeat both cache classes, so RimZ subtracts them before calculating fresh input and adds one idempotent API-equivalent local price per generation. The plain-dollar cumulative total participates in live agent and room budgets, while Cursor provider billing, account-day spend, and historical `rimz stats` totals remain unavailable. `afterAgentResponse` supplies safe final text, and a bounded terminal-only transcript tail recovers missed turn ends. Full native assistant history and incremental reply streaming remain unavailable because Cursor's JSONL merges visible assistant output with thinking.

Cursor hook installation owns both `~/.cursor/hooks.json` and the managed statusline in `~/.cursor/cli-config.json`. Run `rimz hooks install cursor` to repair either an incomplete hook set or a missing wrapper; dry-run and consent show both diffs, and uninstall restores the prior statusline value.

## Versions

RimZ tracks each agent's own release surface, and behaviour can shift with the agent's version — Codex, for example, moved to daemon-routed hooks at 0.137 and adjusted turn-completion signals through the 0.14x line. RimZ adapts at runtime rather than pinning a hard floor here, and `rimz doctor` reports version drift it detects per agent after an upgrade ([troubleshooting](../guide/troubleshooting.md)). For the exact event surface a given agent version exposes, the authority is that agent's [mapping doc](#per-agent-mappings) and external reference.

## Agents not yet supported

An agent RimZ doesn't recognize runs fine in a pane; it renders as a plain process row rather than an agent card, with no live state or attention routing. New agents land the same way the built-ins here did — one adapter over their verified hook or local-store surface ([adding an agent](../internals/agents/model.md#adding-an-agent)). Two categories are known gaps: **remote agents** with no local pane (a `claude remote-control --spawn` worktree, or a Codex thread started from the web) are tracked but not yet rendered, and an agent whose hooks you declined at the consent gate reports nothing until you wire it with `rimz hooks install`.

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
