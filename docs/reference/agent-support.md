# Agent support

RimZ watches the coding agents you already run — Claude Code, Codex, and the alpha and experimental set (Pi, OpenCode, Antigravity, Copilot, Droid, Cursor, Amp, Kiro, Qwen Code, Kimi, and Grok Build) — through one uniform adapter each. An adapter translates that agent's own hooks, transcripts, and APIs into the vocabulary the rest of RimZ speaks, so `rimz agents` launches, `rimz message` steers, and `rimz agents … -p` scripts every built-in that exposes it, all through the same boundary. It reads what the agent does and classifies it; you answer in the agent's own UI, the CLI runs stock, and the official web, desktop, and mobile apps keep working untouched. The boundary in depth is [the agent model](../internals/agents/model.md).

Read the support level honestly. **Claude Code and Codex are the supported daily drivers** — wired end to end and run constantly. **Pi and OpenCode are alpha**, close behind the daily drivers; the remaining agents are experimental, wired against their documented hook and transcript surface and covered by tests but not yet dogfooded enough by the author. Treat alpha and experimental integrations as best-effort: run them anyway, since they mostly just work, and please report the bugs you hit. Support tier tracks lived confidence, not mechanical breadth — an early-tier agent can still wire up a wide surface, as the matrix below shows.

Every integration is declared cell by cell, not assumed. Each adapter states its own coverage, conformance tests cross-check that declaration against the code that backs it, and `rimz coverage` prints the same matrices this page annotates — so what RimZ claims to read is a thing you verify on your own machine rather than take on faith:

```sh
rimz coverage          # the wired / partial / unsupported grid, per agent, with a reason on every cell
rimz coverage --json   # the same, machine-readable
```

## What the marks mean

A mark answers one question: **what do you see, and when do you see it?** Two tests settle every cell.

- **Complete** — the whole capability arrives, rather than a usable slice of it.
- **Live** — it arrives while the agent works, rather than after the turn lands.

| Mark | Meaning |
| :--: | --- |
| ● **full** | Complete and live. The capability reads the way it does on Claude Code. |
| ◐ **partial** | You get a working version with a stated limit: part of the detail, or the whole of it a beat late. The matrix names which. |
| ✗ **unsupported** | The agent surfaces nothing RimZ can render for this capability. |

How RimZ obtains a figure stays out of the mark. A value read from a wrapped statusline and a value read from a transcript tail both count as full when the card shows the same complete, live result; a native hook that reports half the story counts as partial. The mechanism behind each mark is the [wiring matrix](#the-wiring-matrix) below.

## The six capabilities

**State** — the sidebar card tracks the agent's whole life. It appears when a session starts, follows working, waiting, and idle as the agent moves, and clears when the session ends. This is the baseline every other capability sits on: attention routing, card ranking, and message delivery all read it.

- ● Every transition lands, and the card matches the pane.
- ◐ The card is usually right and drifts at some transitions — a lifecycle read out of a local store rather than reported, so a cancel or a failure can read as an ordinary stop.
- ✗ No live state; the pane renders as a plain process row.

What separates the two is whether the agent reports its transitions or RimZ infers them. Several agents publish no session-end event, and their card clears on the next sidebar refresh once the pane is gone; that final tombstone rides the same tick every card already refreshes on, so it stays full. A lifecycle RimZ has to reconstruct wholesale is the partial case.

**Live** — what the card shows while a turn runs: context-window fill, the token breakdown behind it (input, output, cache), and a dollar figure for the work in flight.

- ● All three, moving during the turn.
- ◐ Some of it: a fill percentage without token counts, totals without the breakdown, an estimated price rather than a billed one, or figures that settle only at the turn boundary.
- ✗ Nothing live; the card carries state without numbers.

**History** — everything the agent already did: past sessions readable end to end, with per-turn tokens and dollars feeding [`rimz stats`](../guide/insight.md), the provider dashboard, and the heatmap.

- ● Transcripts, tokens, and dollars, complete across sessions and counted in the spend dashboard.
- ◐ Part of the record: past sessions read back fine while the dollars are estimated locally rather than billed, or absent entirely so the agent contributes nothing to the `rimz stats` totals.
- ✗ The agent keeps no archive RimZ can read.

A partial here is the one users notice most, because it splits reading from accounting: `rimz agents logs` and `rimz agents history` still replay the session, while the provider dashboard shows that agent's session count with the token and dollar positions left blank.

**Account** — who you are signed in as, which plan you are on, and how much of it remains: the named windows (a 5-hour and weekly pair, monthly credits, a prepaid balance) with their fill and reset time.

- ● Plan and windows together, with usage counted against them.
- ◐ Identity and plan with no usage, windows with no plan, or a quota the provider publishes as display-only.
- ✗ No readable account surface.

**Ask** — the moment an agent stops and needs you. The card raises Waiting and the cockpit counts it, so the fleet tells you someone is blocked.

- ● The question itself travels: its text and options reach [`rimz asks`](./cli/asks.md) and the card, so you read what is being asked without opening the pane.
- ◐ The card raises Waiting and routes you to the pane, where the agent's own UI holds the question; `rimz asks` stays empty.
- ✗ A blocked agent looks like a working one.

Answering is a separate surface. Every agent takes its answer in its own UI, and `rimz answer` adds an out-of-band path only where the agent exposes one safely ([asks](./cli/asks.md)).

**Subagents** — child agents nested under the parent's card, so a fan-out reads as one block instead of a mystery pause.

- ● Children appear as they start and update as they work, each carrying its name, task, and model.
- ◐ Children arrive late — often only when the parent's turn ends — or land without part of that detail.
- ✗ Children stay invisible; the parent shows a long turn with no explanation.

Claude Code is the richest of these: its children also carry a running token count and an elapsed clock, and the child row paints both wherever an agent reports them.

## The compatibility matrix

One row per agent, ordered by support tier — Claude and Codex, then the alpha and experimental sets.

| Agent | State | Live | History | Account | Ask | Subagents |
| --- | :--: | :--: | :--: | :--: | :--: | :--: |
| Claude Code | ● | ● | ● | ● | ● | ● |
| Codex | ● | ● | ● | ● | ● | ● |
| Pi | ● | ● | ● | ● | ● | ● |
| OpenCode | ● | ● | ● | ● | ● | ● |
| Antigravity | ● | ◐ | ◐ | ● | ◐ | ◐ |
| Copilot | ● | ◐ | ◐ | ◐ | ● | ◐ |
| Droid | ● | ◐ | ◐ | ✗ | ◐ | ✗ |
| Cursor | ● | ◐ | ◐ | ◐ | ◐ | ◐ |
| Amp | ● | ◐ | ◐ | ◐ | ● | ✗ |
| Kiro | ◐ | ◐ | ◐ | ✗ | ◐ | ✗ |
| Qwen | ● | ◐ | ◐ | ◐ | ● | ● |
| Kimi | ● | ◐ | ◐ | ● | ● | ◐ |
| Grok | ● | ◐ | ● | ◐ | ● | ● |

<sub>● full · ◐ partial · ✗ unsupported. Run `rimz coverage` for the same grid with the exact limit spelled out on every ◐ and ✗ cell.</sub>

Claude Code is the reference integration and reads full across all six; each other agent exposes less of itself to a local observer. Breadth here is independent of support tier — an experimental agent can carry a wide surface, and a daily driver can leave a cell partial by choice. A ✗ is a declared absence the sidebar and `rimz doctor` read from the same place, so a missing surface renders as a stated gap rather than a silent bug.

## The wiring matrix

Under the six capabilities sits the mechanism: seventeen integration concerns naming what each adapter reads from its agent. This is the grid to consult when a capability reads partial and you want the specific reason, or when you are [building an adapter](../contributing/agent-adapters.md).

A cell reads **wired** (✓, the concern reaches a user-complete state), **partial** (◐, coverage is incomplete and the adapter names the gap), or **unsupported** (✗, unreachable from the agent's current protocol).

| Agent | `turn` | `perm` | `plan` | `ask` | `answer` | `compact` | `sub` | `bg` | `end` | `idle` | `usage` | `live$` | `rich` | `install` | `spend` | `tools` | `remote` |
| --- | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: |
| Claude | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Codex | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✗ | ◐ | ◐ | ✓ | ✓ | ✓ | ✓ | ✓ | ◐ | ✓ |
| Pi | ✓ | ✗ | ✗ | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ | ◐ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✗ |
| OpenCode | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ | ✓ | ✗ | ✓ | ◐ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✗ |
| Antigravity | ✓ | ◐ | ✗ | ◐ | ✗ | ✗ | ◐ | ✓ | ◐ | ◐ | ✓ | ◐ | ✓ | ✓ | ✗ | ✗ | ✗ |
| Copilot | ✓ | ✓ | ✗ | ✓ | ✗ | ◐ | ◐ | ✗ | ✓ | ◐ | ✓ | ◐ | ✓ | ✓ | ◐ | ✗ | ✗ |
| Droid | ✓ | ✗ | ✗ | ◐ | ✗ | ✓ | ✗ | ✗ | ✓ | ✓ | ✓ | ◐ | ◐ | ✓ | ✗ | ✗ | ✗ |
| Cursor | ✓ | ✗ | ◐ | ◐ | ✗ | ◐ | ◐ | ✗ | ✓ | ◐ | ✓ | ◐ | ✓ | ✓ | ✗ | ✗ | ✗ |
| Amp | ✓ | ✓ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ◐ | ◐ | ◐ | ◐ | ✗ | ✓ | ◐ | ✗ | ✗ |
| Kiro | ◐ | ◐ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ◐ | ◐ | ◐ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ |
| Qwen | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ◐ | ✓ | ✓ | ◐ | ✗ | ✗ |
| Kimi | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ | ◐ | ✗ | ✓ | ◐ | ◐ | ✓ | ✗ | ✓ | ◐ | ✗ | ✗ |
| Grok | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ | ✓ | ✗ | ✓ | ◐ | ✓ | ◐ | ◐ | ✓ | ✓ | ✗ | ✗ |

<sub>✓ wired · ◐ partial (derived) · ✗ unsupported. `rimz coverage --wiring` prints this grid with the exact gap on every cell.</sub>

What each concern drives: `turn` live status (session start and every turn boundary), `perm` permission prompts routed to your keyboard, `plan` a plan-approval gate raising a waiting row, `ask` the agent's ask-the-user tool raising a waiting row, `answer` structured answers driving supported native prompt actions, `compact` context compaction on the card, `sub` the subagent tree as nested rows, `bg` a turn parked on background work, `end` the card tombstoning when the session closes, `idle` an idle nudge when the agent goes quiet, `usage` context-window fill and token counts, `live$` the live dollar figure, `rich` provider extras (official model labels, account windows), `install` RimZ installing the reporting hooks, `spend` account spend for the [token-insight](../guide/insight.md) dashboard, `tools` live and historical named tool-call counts, and `remote` driving or spawning a session with no local pane.

Identical-tool loop detection extends `tools` only when the adapter also exposes structured tool arguments, because a name-only run would misclassify legitimate reads. Grok emits unnamed tool observations and Kiro exposes no tool-input key, so both keep tool-loop detection off.

### Notes on the alpha and experimental set

The gaps you will actually feel, per agent. Each agent's [mapping doc](#per-agent-mappings) carries the full rationale.

- **Antigravity** reads full on State and Account and carries a wide surface for an experimental agent: lifecycle is fully hooked, and plan plus the `5h` and weekly bars come from a read-only local service. Its four partials are each one specific limit. Permissions and questions stay in Antigravity's own UI, because RimZ deliberately installs no permission hook, so an open prompt raises the waiting card and routes you to the pane. Live context and tokens run current off a wrapped statusline while the dollar figure prices the current turn rather than a session total, which also keeps it out of room spend and budget caps. History replays every past session, and contributes no dollars to `rimz stats`. Children join to the parent's ordered `invoke_subagent` transcript records, which the CLI flushes late, so a fan-out often lands as the parent's turn ends. Every Antigravity error stop is terminal, so a supervised run does not survive a provider limit and auto-continue never arms. `rimz hooks install antigravity --dry-run` previews the changes before consent, and uninstall restores the prior statusline.
- **Copilot** reports the concrete auto-selected model, effort, occupied-window tokens, current-call composition, and normalized cumulative session tokens through its wrapped statusline; those totals produce an estimated live dollar figure at the same resolved model, while metadata-only OTel remains the fallback when the bridge is absent or replaced. Finalized shutdown history adds per-model tokens and locally estimated dollars to `rimz stats` and the provider dashboard without claiming an authoritative billing ledger. Singular and batched ask hooks raise Waiting, and every post-tool completion clears it immediately after the native answer. Standard child prompt/stop hooks join to the parent's transcript records by exact task-call ID, publishing the model at start and the exact total after completion; child tools and permissions remain unavailable. Its bounded account query adds the plan and named monthly `cr`/`cht` plus genuine `prm` bars without inventing 5h/7d windows; statusline `ai_used`, IDE completions, extra credits, authoritative account dollars, and remote control remain unsupported. Install previews and manages both the hook file and `$COPILOT_HOME/settings.json` and restores a prior user statusline on uninstall.
- **Cursor** exposes its official `AskQuestion` and plan-approval UIs without corresponding hooks. RimZ derives an open synchronous question or **Ready to build?** plan proposal from Cursor's version-pinned local state, raises a pane-only waiting card, and routes you to the existing pane; `rimz asks` stays empty and `rimz answer` remains unsupported because the native UI is the only safe answer surface. A later conversation message clears the plan wait, while dismissing it with Esc or `p` leaves the waiting card until the next turn because Cursor writes no store change. Subagents are partial: the pinned CLI defines and accepts `subagentStart`/`subagentStop` hooks but never issues those requests, so RimZ derives exact child/parent lifecycle from the chats store and child transcript when the next parent hook feeds, often only at turn end. Live context rides its statusline, and RimZ prices each generation locally; that running session total counts toward live agent and room [budgets](../guide/budget.md), while provider billing, account spend, and `rimz stats` stay unavailable. The install manages `~/.cursor/hooks.json` and the statusline in `~/.cursor/cli-config.json`, shows both diffs before consent, and restores the prior statusline on uninstall.
- **Droid** misses the same ask wire, but RimZ derives the waiting card from the transcript's active `AskUser` call; the answer still happens in Droid's pane. Its locally priced session total reaches the card and live budgets the way Cursor's does, while provider dollars, historical spend, and quota stay unavailable.
- **Kiro** did not execute its documented hooks under verification, so RimZ pulls its lifecycle from Kiro's local session store instead. A pending tool approval still marks the card waiting, but `rimz asks` and `rimz answer` do not claim it; context is percentage-only, and hook install and supervised `-p` runs are unsupported.
- **Kimi** joins `SubagentStart` and `SubagentStop` previews to validated child entries in `state.json` and their `wire.jsonl` records, namespacing Kimi's per-session child counters before creating nested rows. Resumed children and ambiguous concurrent starts expose no start-time identity and therefore appear only when the response-side Stop match becomes unique. Child-fired session `Stop` hooks are suppressed while the main wire is provably mid-step, so the parent stays running until its own final Stop.
- **Qwen** is the one adapter whose quota is scoped to an exact provider account rather than to the CLI. A fresh supervised or loop launch binds the final region and API key to an opaque account fingerprint and then reads only that account's cached Coding Plan windows, so a spent window stops the launch before a run record, pane, or loop check exists, and a cached window belonging to a different account cannot stop it. Interactive launches, resume, fork, wake, and mid-run auto-continue stay outside the boundary. What it does to your runs is [budgets → one model, five scopes](../guide/budget.md#one-model-five-scopes).
- **Grok** installs only passive global hooks, normalizes Grok's snake_case event values before lifecycle dispatch, and leaves every permission decision in the native TUI. Exact `Notification` hooks create the durable permission, plan, diff-review, and question asks; when a Grok version records only an unmatched permission request in its validated `events.jsonl` sibling, RimZ raises a display-only waiting card that keeps `rimz asks` empty and the pane as the answer surface. Its rewind-aware `updates.jsonl` fold supplies full history, child lifecycle, completed-turn context detail, and native or locally priced completed-turn dollars; live mid-turn cost and account quota windows remain unavailable. Ask uses `--permission-mode default`, Auto uses `--permission-mode auto`, Yolo uses `--yolo`, and Plan intentionally adds no argv because `/plan` is interactive rather than an enforced launch posture.

## The lifecycle hook surface

Under the concern matrix sits the raw event surface: the eleven lifecycle signals RimZ folds into every agent's state machine, and the native event each agent fires for each one. `rimz coverage` prints this as its second grid, the hooks matrix; here it is with the native event names in place, in the same support-tier order.

| Agent | `registered` | `turn_started` | `turn_ended` | `tool_used` | `awaiting_input` | `subagent_started` | `subagent_stopped` | `compacting` | `compaction_ended` | `ended` | `lost` |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Claude | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `PermissionRequest` | `SubagentStart` | `SubagentStop` | `PreCompact` | `PostCompact` | `SessionEnd` | ◐ derived |
| Codex | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `PermissionRequest`; `Stop` + rollout `Plan` | `SubagentStart` | `SubagentStop` | `PreCompact` | `PostCompact` | ◐ derived | ◐ derived |
| Pi | `session_start` | `before_agent_start` | `agent_settled` (`agent_end` before Pi 0.80.4) | `tool_execution_end` | `tool_call` | `subagent_started` | `subagent_stopped` | `session_before_compact` | `session_compact` | `session_shutdown` | ◐ derived |
| OpenCode | `session_created` | `chat_message` | `session_idle` | `tool_after` | `permission_ask`; `session_idle` + plan turn | `SubagentStart` | `SubagentStop` | `session_compacting` | `session_compacted` | `session_ended` | ◐ derived |
| Antigravity | ◐ first `PreInvocation` identity + local discovery | `PreInvocation` | `Stop` | `PostToolUse` | ◐ statusline permission marker + transcript question | ◐ child `PreInvocation` + parent transcript join | ◐ child `Stop` + parent transcript join | ✗ | ✗ | ◐ derived | ◐ derived |
| Copilot | `sessionStart` | `userPromptSubmitted` | `agentStop` | `postToolUse` | `permissionRequest` | ◐ child `userPromptSubmitted` + parent transcript join | ◐ child `agentStop` + parent transcript join | `preCompact` | ◐ derived | `sessionEnd` | ◐ derived |
| Droid | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | ◐ transcript `AskUser` | ✗ | ✗ | `PreCompact` | `SessionStart:compact` | `SessionEnd` | ◐ derived |
| Cursor | `sessionStart` | `beforeSubmitPrompt` | `stop` | `postToolUse` | ◐ local pending `AskQuestion` or plan proposal | `subagentStart` | `subagentStop` | `preCompact` | ◐ derived | `sessionEnd` | ◐ derived |
| Amp | `session_start` | `agent_start` | `agent_end` | `tool_result` | `permission_ask` | ✗ | ✗ | ✗ | ✗ | ◐ derived | ◐ derived |
| Kiro | ◐ local store | ◐ `turn_start` | ◐ `turn_end` | ◐ tool records | ◐ pending interaction | ✗ | ✗ | ✗ | ✗ | ◐ derived | ◐ derived |
| Qwen | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `PermissionRequest` | `SubagentStart` | `SubagentStop` | `PreCompact` | `PostCompact` | `SessionEnd` | ◐ derived |
| Kimi | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `PermissionRequest` | ◐ `SubagentStart` + durable child join | ◐ `SubagentStop` + durable child join | `PreCompact` | `PostCompact` | `SessionEnd` | ◐ derived |
| Grok | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `Notification` | `SubagentStart` | `SubagentStop` | `PreCompact` | `PostCompact` | `SessionEnd` | ◐ derived |

`lost` — an agent's mux-session dying out from under it — has no native event in any built-in, because an agent's own hooks stop firing exactly when the thing that would report the death is gone. RimZ derives it from the `rimz exec` launch wrapper instead. Where `ended` is derived (Codex, Antigravity, Amp, Kiro), the same pane-liveness-and-reaper path clears the row on the next snapshot tick rather than at the instant of exit.

## Per-agent mappings

The detail for each agent — its full coverage rationale, permission-mode mapping, effort levels, install target, resume/fork surface, and account probing — lives in that agent's mapping doc, with its upstream protocol in the matching external reference. Adding an agent means implementing only its supported workflow capabilities, declaring an `AgentSpec`, and composing one registry definition ([adding an agent](../internals/agents/adapter.md#adding-an-agent)).

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

An agent RimZ doesn't recognize runs fine in a pane; it renders as a plain process row rather than an agent card, with no live state or attention routing. New agents land the same way the built-ins here did — one adapter over their verified hook or local-store surface ([adding an agent](../internals/agents/adapter.md#adding-an-agent)). Two categories are known gaps: **remote agents** with no local pane (a `claude remote-control --spawn` worktree, or a Codex thread started from the web) are tracked but not yet rendered, and an agent whose hooks you declined at the consent gate reports nothing until you wire it with `rimz hooks install`.

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
