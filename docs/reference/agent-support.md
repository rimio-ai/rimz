# Agent support

RimZ has a built-in adapter for thirteen coding agents. Each adapter reads the agent's own hooks, transcripts, and local files, and turns them into the sidebar card, `rimz asks`, spend figures, and launch argv. The agent CLI runs stock, you answer it in its own UI, and the provider's web, desktop, and mobile apps keep working. This page answers, per agent: what the card shows, which files and flags RimZ touches, and where the gaps are. How the adapter boundary works is [the agent model](../internals/agents/model.md).

Each agent sits in a support tier. The tier is how much the author runs it day to day; every agent in every tier is wired against its documented surface and covered by tests. An experimental agent can still carry a wide surface, as the [compatibility matrix](#the-compatibility-matrix) shows.

| Tier | Agents | What to expect |
| --- | --- | --- |
| Supported | Claude Code, Codex | Wired end to end and run constantly. |
| Alpha | Pi, OpenCode | Close behind the supported pair. |
| Experimental | Antigravity, Copilot, Droid, Cursor, Amp, Kiro CLI, Qwen Code, Kimi Code, Grok Build | Mostly works; expect the occasional bug and [report it](https://github.com/rimio-ai/rimz/issues). |

RimZ tracks each agent's latest release. Where a feature needs a minimum agent version, this page names it next to the feature.

## Check coverage on your machine

`rimz coverage` prints the capability grid with the limit behind every cell, and `rimz coverage --wiring` adds the [wiring matrix](#the-wiring-matrix) and the [lifecycle hook grid](#the-lifecycle-hook-surface). Every adapter declares its own coverage, and a test holds the declarations to the matrices on this page. The command's output, flags, and JSON shape are in [check adapter coverage](./cli/maintenance.md#check-adapter-coverage).

The terminal grids differ from this page in two ways. Cells print `✓` where this page shows ● or ✓, `!` where it shows ◐, and `✗` for ✗. Rows follow the adapter registry (claude, codex, amp, copilot, kimi, pi, opencode, antigravity, cursor, droid, kiro, qwen, grok) instead of tier order, and loaded [process plugins](#third-party-plugins) follow the built-ins.

## What the marks mean

A mark says what you see on the card and when you see it. Two tests settle every cell: whether the whole capability arrives (complete), and whether it arrives while the agent works rather than after the turn ends (live).

| Mark | Meaning |
| :--: | --- |
| ● full | Complete and live. The capability reads the way it does on Claude Code. |
| ◐ partial | A working version with a stated limit: part of the detail, or all of it late. `rimz coverage` names the limit. |
| ✗ unsupported | The agent exposes nothing RimZ can show for this capability. |

How RimZ gets a figure does not change its mark. A value read from a wrapped statusline and one read from a transcript tail both count as full when the card shows the same complete, live result; a native hook that carries half the story counts as partial. The mechanism is the [wiring matrix](#the-wiring-matrix).

## The six capabilities

Each capability is one thing a card or command shows you. State is the base the rest depend on: attention routing, card ranking, and message delivery all read it.

| Capability | What it covers | ● full | ◐ partial | ✗ unsupported |
| --- | --- | --- | --- | --- |
| State | The card appears at session start, follows working, waiting, and idle, and clears when the session ends. | Every transition lands, and the card matches the pane. | RimZ reads the lifecycle from a local store instead of being told, so the card can drift: a cancel or failure can read as an ordinary stop. | No live state; the pane shows as a plain process row. |
| Live | While a turn runs: context-window fill, the token breakdown (input, output, cache), and a dollar figure. | All three, moving during the turn. | Some of it: a fill percentage without token counts, totals without the breakdown, an estimated price instead of a billed one, or figures that update only at the turn boundary. | No numbers; the card carries state only. |
| History | Past sessions readable end to end, with per-turn tokens and dollars feeding [`rimz stats`](./cli/stats.md), the provider dashboard, and the heatmap. | Transcripts, tokens, and dollars, complete across sessions. | Sessions read back, but dollars are estimated locally or missing. | No archive RimZ can read. |
| Account | Your login, plan, and usage windows (a 5-hour and weekly pair, monthly credits, a prepaid balance) with fill and reset time. | Plan and windows, with usage counted against them. | Identity and plan with no usage, windows with no plan, or a quota the provider publishes as display-only. | No readable account surface. |
| Ask | An agent stops and needs you: the card raises Waiting and the cockpit counts it. | The question's text and options reach [`rimz asks`](./cli/asks.md) and the card, so you can read it without opening the pane. | The card raises Waiting and routes you to the pane, where the question stays in the agent's UI; `rimz asks` stays empty. | A blocked agent looks like a working one. |
| Subagents | Child agents nested under the parent card. | Children appear as they start and update as they work, with name, task, and model. | Children arrive late (often when the parent's turn ends) or without part of that detail. | Children stay invisible; the parent shows a long turn. |

A few cells need more than the table says:

- State stays full for agents that publish no session-end event. Their card clears on the next sidebar refresh after the pane is gone, the same tick every card refreshes on.
- A History partial splits reading from accounting. `rimz agents logs` and `rimz agents history` still replay the session, while the provider dashboard shows the agent's session count with tokens and dollars blank.
- Answering is separate from Ask. Every agent takes its answer in its own UI, and `rimz answer` adds an out-of-band path for Claude, Codex, and Pi ([what each agent accepts](./cli/asks.md#what-each-agent-accepts)).
- Claude Code children also carry a running token count and an elapsed clock. The child row shows both for any agent that reports them.

## The compatibility matrix

One row per agent, in tier order. Run `rimz coverage` for the limit behind every ◐ and ✗.

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

<sub>● full · ◐ partial · ✗ unsupported</sub>

A ✗ is a declared absence with a stated reason in `rimz coverage`, so a missing surface is a known gap and not a bug.

## Notes on the alpha and experimental set

These are the gaps you will notice, per agent, beyond what the matrix and `rimz coverage` state. The files `rimz hooks install` writes for each agent, and what uninstall restores, are in [what install writes](./cli/hooks-trust.md#what-install-writes). Each agent's [mapping doc](#per-agent-mappings) has the full rationale.

### Antigravity

- Permission prompts and questions stay in Antigravity's UI, because RimZ installs no permission hook. An open prompt raises Waiting and routes you to the pane.
- Context and tokens are live from the wrapped statusline. The dollar figure prices the current turn only, so it does not count toward room spend or [budget caps](./cli/budget.md).
- History replays every past session and adds no dollars to `rimz stats`.
- Children come from the parent's `invoke_subagent` transcript records, which the CLI writes late, so a fan-out often appears as the parent's turn ends.
- Every error stop is terminal: a supervised run does not survive a provider limit, and [auto-continue](../guide/loops.md#auto-continue) never arms.

### Copilot

- The wrapped statusline supplies the resolved model, effort, context tokens, and cumulative session tokens. The live dollar figure comes from the session's AI credits at $0.01 per credit, or is an estimate at that model when credits are absent; when the statusline is missing or replaced, RimZ falls back to OpenTelemetry metadata.
- History adds per-model tokens and credit-metered dollars (local estimates where credits are absent) to `rimz stats` and the provider dashboard. These are not an account billing ledger.
- Questions raise Waiting and clear as soon as the tool that asked completes.
- Children appear with their model when they start, show their tool activity, and report their exact token total when they finish. A child's permission prompt arrives on the parent session, so the parent card waits until the parent's next hook.
- The account shows the plan and the monthly `cr`, `cht`, and `prm` windows. There are no 5-hour or weekly windows, and IDE completions, extra credits, account dollars, and remote control are unsupported.

### Cursor

- Cursor's `AskQuestion` and plan-approval ("Ready to build?") prompts have no hooks. RimZ detects an open one from Cursor's local state, raises Waiting, and routes you to the pane; `rimz asks` stays empty and `rimz answer` is unsupported.
- A later message clears a plan wait. Dismissing the plan prompt with Esc or `p` leaves the card waiting until the next turn, because Cursor records no change.
- The installed CLI accepts `subagentStart` and `subagentStop` hooks but never fires them. RimZ reads children from the chats store when the parent's next hook fires, often at turn end.
- RimZ prices each generation locally. The running session total counts toward agent and room [budgets](./cli/budget.md); provider billing, account spend, and `rimz stats` dollars are unavailable.

### Droid

- Droid has no ask hook. RimZ raises Waiting from the transcript's active `AskUser` call, and you answer in the pane.
- The locally priced session total reaches the card and live budgets. Provider dollars, historical spend, and quota are unavailable.

### Kiro

- Kiro's documented hooks did not run under verification, so RimZ reads the lifecycle from Kiro's local session store.
- A pending tool approval raises Waiting; `rimz asks` and `rimz answer` do not see it.
- Context is a percentage only.
- `rimz hooks install kiro` and supervised `-p` runs are unsupported, and [`rimz agents compact`](./cli/agents.md#compact) refuses Kiro because it has no native turn-start hook.

### Kimi

- Children resumed from an earlier session, and children started at the same moment, appear only when they stop.
- A child's `Stop` hook does not end the parent's turn: the parent stays running until its own final `Stop`.

### Qwen

- Qwen's quota is scoped to the exact provider account (region and API key), not to the CLI. A fresh supervised or loop launch reads only that account's cached Coding Plan windows: a spent window stops the launch before any pane or run record exists, and another account's window cannot stop it.
- Interactive launches, resume, fork, wait, and auto-continue ignore the quota. See [budgets: one model, five scopes](../guide/budget.md#one-model-five-scopes).

### Grok

- RimZ installs passive global hooks only; every permission decision stays in Grok's TUI.
- Permission, plan, diff-review, and question prompts reach `rimz asks` through Grok's `Notification` hook. When a Grok version logs only an unmatched permission request, the card waits with `rimz asks` empty and the pane as the answer surface.
- Dollars land at each completed turn, native or locally priced. Mid-turn cost and account quota windows are unavailable.

## Config homes and skills

Each built-in agent keeps its settings, sessions, and credentials in a config home. RimZ reads from it, installs hooks into it, and binds it into the [sandbox](../guide/configuration.md#agent-isolation).

| Agent | Config home | Override | Skill root | Sandbox `skills` list |
| --- | --- | --- | --- | --- |
| Claude Code | `~/.claude` | first `CLAUDE_CONFIG_DIR` entry | `<home>/skills` | accepted |
| Codex | `~/.codex` | `CODEX_HOME` | `~/.agents/skills` | accepted |
| Pi | `~/.pi/agent` | `PI_CODING_AGENT_DIR` | `~/.agents/skills` | accepted |
| OpenCode | `~/.config/opencode` | `XDG_CONFIG_HOME` + `/opencode` | `~/.agents/skills` | refused |
| Antigravity | `~/.gemini/antigravity-cli` | none | `~/.agents/skills` | refused |
| Copilot | `~/.copilot` | `COPILOT_HOME` | `~/.agents/skills` | accepted |
| Droid | `~/.factory` | none | `~/.agents/skills` | accepted |
| Cursor | `~/.cursor` | `CURSOR_CONFIG_DIR`, else `XDG_CONFIG_HOME/cursor` on Linux and BSD | `~/.agents/skills` | accepted |
| Amp | `~/.config/amp` | `XDG_CONFIG_HOME` + `/amp` | `~/.agents/skills` | refused |
| Kiro | `~/.kiro` | `KIRO_HOME` | `<home>/skills` | refused |
| Qwen | `~/.qwen` | `QWEN_HOME` | `<home>/skills` | accepted |
| Kimi | `~/.kimi-code` | `KIMI_CODE_HOME` | `~/.agents/skills` | accepted |
| Grok | `~/.grok` | `GROK_HOME` | `~/.agents/skills` | refused |
| Process plugin | none | none | none | refused |

Claude Code and Codex also run under named accounts: RimZ sets the override variable to a separate config home per account when a room launches that provider. Other agents always use their default home. See [provider accounts](../guide/accounts.md).

Under `agents.isolation = "sandbox"` (Linux), RimZ binds each agent's config home from the launch environment and merges the RimZ skill library into the agent's skill root. The sandbox does not hide the rest of the host or move credentials. Project skills and Codex's `$CODEX_HOME/skills` stay outside the merged view. A profile `skills` list keeps the listed skills model-callable and makes the rest user-only; agents marked refused in the table fail the launch when a list is set. Host isolation ignores every `skills` list, including `[]`, and duplicate or invalid names are parse errors under both. The profile rules are in [configuration: profiles](../guide/configuration.md#profiles), and mount order and markers in [sandbox internals](../internals/sandbox.md#profile-skill-views).

## Launch flags

RimZ launches each agent with the provider's own flags. A typed profile field or `rimz agents` flag that an agent cannot express fails the launch before any pane opens, with an error that the agent "does not support profile field" and names the field. Raw profile `args` pass any other provider flag through unchanged.

### Permission modes

A permission mode comes from the `mode` profile field, the `--ask` and `--yolo` flags, or a `<kind>-<mode>` cell such as `claude-plan`. RimZ appends the arguments below. `none` means the mode adds no arguments, so the agent keeps its own default; for Auto and Yolo, the `<kind>-auto` and `<kind>-yolo` cells do not exist ([permission-mode cells](./cli/agents.md#permission-mode-cells)).

| Agent | Ask | Auto | Plan | Yolo |
| --- | --- | --- | --- | --- |
| Claude Code | none | `--permission-mode auto` | `--permission-mode plan` | `--dangerously-skip-permissions` |
| Codex | none | `--ask-for-approval never --sandbox workspace-write` | none | `--dangerously-bypass-approvals-and-sandbox` |
| Pi | none | none | none | none |
| OpenCode | none | none | `--agent plan` | `--auto` |
| Antigravity | none | `--mode accept-edits` | `--mode plan` | `--dangerously-skip-permissions` |
| Copilot | none | `--autopilot` | `--plan` | `--allow-all` |
| Droid | none | `--auto medium` | `--use-spec` | none |
| Cursor | none | `--auto-review` | `--mode=plan` | `--force --sandbox disabled` |
| Amp | none | none | none | none |
| Kiro | none | none | none | none |
| Qwen | none | `--approval-mode auto-edit` | `--approval-mode plan` | `--approval-mode yolo` |
| Kimi | none | `--auto` | `--plan` | `--yolo` |
| Grok | `--permission-mode default` | `--permission-mode auto` | none | `--yolo` |
| Process plugin | declared by bundle | declared by bundle | declared by bundle | declared by bundle |

Grok Plan adds no arguments because Grok's `/plan` is an interactive command with no launch flag.

### Model and effort

`--model` and `--effort` on `rimz agents`, and the `model` and `effort` profile fields, render as below. RimZ passes the value through without checking it, so effort levels are whatever the agent's CLI accepts.

| Agent | Model | Effort |
| --- | --- | --- |
| Claude Code | `--model <model>` | `--effort <level>` |
| Codex | `--model <model>` | `-c model_reasoning_effort=<level>` |
| Pi | `--model <model>` | `--thinking <level>` |
| OpenCode | `--model <model>` | refused |
| Antigravity | `--model <model>` | refused |
| Copilot | `--model <model>` | `--effort <level>` |
| Droid | refused | refused |
| Cursor | `--model <model>` | refused |
| Amp | `--mode <model>` | refused |
| Kiro | `--model <model>` | `--effort <level>` |
| Qwen | `--model <model>` | refused |
| Kimi | `--model <model>` | refused |
| Grok | `--model <model>` | `--reasoning-effort <level>` |
| Process plugin | declared by bundle | declared by bundle |

Amp has no model flag; its `--mode` selects the agent mode, and the `model` value is passed there. The `--max-turns` cap for supervised runs is in [supervised runs](./cli/agents.md#supervised-runs--p).

### Launch-prompt replacement

The profile fields `system-prompt-file` and `append-system-prompt-files` (and the matching `rimz agents` flags) replace the agent's system prompt. RimZ joins the base file and the appended files in order into one prompt, then hands it to the agent.

| Agent | Replacement | How the agent receives it |
| --- | :--: | --- |
| Claude Code | ✓ | `--system-prompt-file <path>` |
| Codex | ✓ | `-c model_instructions_file=<path>` |
| Pi | ✓ | the prompt text in `--system-prompt`, at most 120 KiB |
| Qwen | ✓ | the composed file's path in `QWEN_SYSTEM_MD` |
| Droid | ✗ | Droid only appends; pass its flag through profile `args` |
| OpenCode, Antigravity, Copilot, Cursor, Amp, Kiro, Kimi, Grok | ✗ | no verified replacement flag |
| Process plugin | declared by bundle | the composed file's path in `[launch].system-prompt-file-flag` |

Pi's replacement is not complete: Pi still appends its own `APPEND_SYSTEM.md`, context files, skills, and working-directory material after RimZ's text. The full prompt is visible in Pi's process arguments.

### Auto-compaction window

The `auto-compact` field on a profile or team role sets the agent's native auto-compaction window, as a token count from 100k through 1M inclusive: `"200k"`, `"200000"`, or `"1m"`. RimZ rejects anything outside that range before launch, so `"200"` fails instead of meaning something different to each CLI. This is separate from [RimZ smart compaction](../guide/configuration.md#smart-compaction).

| Agent | Auto-compaction window | How the agent receives it |
| --- | :--: | --- |
| Claude Code | ✓ | `--autocompact <tokens>`; needs Claude Code 2.1.221 or later, and Claude caps it at the model's context window |
| Codex | ✓ | `-c model_auto_compact_token_limit=<tokens>`; Codex clamps it to 90% of the model window |
| Every other agent and process plugins | ✗ | refused at launch |

## The wiring matrix

The wiring matrix is the mechanism under the six capabilities: eighteen integration concerns, each naming one thing an adapter reads from or drives in its agent. Use it to find why a capability reads partial, or when [building an adapter](../contributing/agent-adapters.md).

| Agent | `turn` | `perm` | `plan` | `ask` | `answer` | `compact` | `sub` | `remind` | `bg` | `end` | `idle` | `usage` | `live$` | `rich` | `install` | `spend` | `tools` | `remote` |
| --- | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: |
| Claude | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Codex | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✗ | ◐ | ◐ | ✓ | ✓ | ✓ | ✓ | ✓ | ◐ | ✓ |
| Pi | ✓ | ✗ | ✗ | ✓ | ✓ | ✓ | ✓ | ✗ | ✗ | ✓ | ◐ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✗ |
| OpenCode | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ | ✓ | ✗ | ✗ | ✓ | ◐ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✗ |
| Antigravity | ✓ | ◐ | ✗ | ◐ | ✗ | ✗ | ◐ | ✗ | ✓ | ◐ | ◐ | ✓ | ◐ | ✓ | ✓ | ✗ | ✗ | ✗ |
| Copilot | ✓ | ✓ | ✗ | ✓ | ✗ | ◐ | ◐ | ✗ | ✗ | ✓ | ◐ | ✓ | ◐ | ✓ | ✓ | ◐ | ✗ | ✗ |
| Droid | ✓ | ✗ | ✗ | ◐ | ✗ | ✓ | ✗ | ✓ | ✗ | ✓ | ✓ | ✓ | ◐ | ◐ | ✓ | ✗ | ✗ | ✗ |
| Cursor | ✓ | ✗ | ◐ | ◐ | ✗ | ◐ | ◐ | ✗ | ✗ | ✓ | ◐ | ✓ | ◐ | ✓ | ✓ | ✗ | ✗ | ✗ |
| Amp | ✓ | ✓ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ◐ | ◐ | ◐ | ◐ | ✗ | ✓ | ◐ | ✗ | ✗ |
| Kiro | ◐ | ◐ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ◐ | ◐ | ◐ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ |
| Qwen | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ◐ | ✓ | ✓ | ◐ | ✗ | ✗ |
| Kimi | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ | ◐ | ✗ | ✗ | ✓ | ◐ | ◐ | ✓ | ✗ | ✓ | ◐ | ✗ | ✗ |
| Grok | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ | ✓ | ✗ | ✗ | ✓ | ◐ | ✓ | ◐ | ◐ | ✓ | ✓ | ✗ | ✗ |

<sub>✓ wired (the concern reaches a user-complete state) · ◐ partial (the adapter names the gap) · ✗ unsupported (out of reach of the agent's protocol). `rimz coverage --wiring` prints the gap behind every cell.</sub>

| Concern | What it drives |
| --- | --- |
| `turn` | Live status from session start and every turn boundary. |
| `perm` | Permission prompts raising a waiting card. |
| `plan` | A plan-approval gate raising a waiting card. |
| `ask` | The agent's ask-the-user tool raising a waiting card. |
| `answer` | `rimz answer` driving the agent's native prompt. |
| `compact` | Context compaction shown on the card. |
| `sub` | Child agents as nested rows. |
| `remind` | RimZ launch reminders reaching the agent's system or developer prompt. |
| `bg` | A turn parked on background work. |
| `end` | The card clearing when the session closes. |
| `idle` | An idle nudge when the agent goes quiet. |
| `usage` | Context-window fill and token counts. |
| `live$` | The live dollar figure. |
| `rich` | Provider extras: official model labels and account windows. |
| `install` | `rimz hooks install` setting up the reporting hooks. |
| `spend` | Account spend for [`rimz stats`](./cli/stats.md) and [account budget caps](./cli/budget.md). |
| `tools` | Named tool-call counts, live and historical. |
| `remote` | Driving or spawning a session with no local pane. |

Identical-tool loop detection needs both the tool name and its structured arguments on the hook, because a name-only match would flag legitimate repeated reads. Only Claude Code and Codex send both; every other agent has loop detection off.

## The lifecycle hook surface

RimZ folds eleven lifecycle signals into every agent's state. The table names the native event each agent fires for each signal; `rimz coverage --wiring` prints the same grid with marks only (`✓` native, `!` derived, `✗` absent). A ◐ cell is derived: RimZ reconstructs the signal from other evidence.

| Agent | `registered` | `turn_started` | `turn_ended` | `tool_used` | `awaiting_input` | `subagent_started` | `subagent_stopped` | `compacting` | `compaction_ended` | `ended` | `lost` |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Claude | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `PermissionRequest` | `SubagentStart` | `SubagentStop` | `PreCompact` | `PostCompact` | `SessionEnd` | ◐ derived |
| Codex | `SessionStart` | `UserPromptSubmit` | `Stop` / `Interrupt` | `PostToolUse` | `PermissionRequest`; `Stop` + rollout `Plan` | `SubagentStart` | `SubagentStop` | `PreCompact` | `PostCompact` | ◐ derived | ◐ derived |
| Pi | `session_start` | `before_agent_start` | `agent_settled` (`agent_end` before Pi 0.80.4) | `tool_execution_end` | `tool_call` | `subagent_started` | `subagent_stopped` | `session_before_compact` | `session_compact`; `session_compact_failed` | `session_shutdown` | ◐ derived |
| OpenCode | `session_created` | `chat_message` | `session_idle` | `tool_after` | `permission_ask`; `session_idle` + plan turn | `SubagentStart` | `SubagentStop` | `session_compacting` | `session_compacted` | `session_ended` | ◐ derived |
| Antigravity | ◐ first `PreInvocation` + local discovery | `PreInvocation` | `Stop` | `PostToolUse` | ◐ statusline permission marker + transcript question | ◐ child `PreInvocation` + parent transcript | ◐ child `Stop` + parent transcript | ✗ | ✗ | ◐ derived | ◐ derived |
| Copilot | `sessionStart` | `userPromptSubmitted` | `agentStop` | `postToolUse` | `permissionRequest` | ◐ child `userPromptSubmitted` + parent transcript | ◐ child `agentStop` + parent transcript | `preCompact` | ◐ derived | `sessionEnd` | ◐ derived |
| Droid | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | ◐ transcript `AskUser` | ✗ | ✗ | `PreCompact` | `SessionStart:compact` | `SessionEnd` | ◐ derived |
| Cursor | `sessionStart` | `beforeSubmitPrompt` | `stop` | `postToolUse` | ◐ local pending `AskQuestion` or plan proposal | `subagentStart` | `subagentStop` | `preCompact` | ◐ derived | `sessionEnd` | ◐ derived |
| Amp | `session_start` | `agent_start` | `agent_end` | `tool_result` | `permission_ask` | ✗ | ✗ | ✗ | ✗ | ◐ derived | ◐ derived |
| Kiro | ◐ local store | ◐ `turn_start` | ◐ `turn_end` | ◐ tool records | ◐ pending interaction | ✗ | ✗ | ✗ | ✗ | ◐ derived | ◐ derived |
| Qwen | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `PermissionRequest` | `SubagentStart` | `SubagentStop` | `PreCompact` | `PostCompact` | `SessionEnd` | ◐ derived |
| Kimi | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `PermissionRequest` | ◐ `SubagentStart` + child session files | ◐ `SubagentStop` + child session files | `PreCompact` | `PostCompact` | `SessionEnd` | ◐ derived |
| Grok | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `Notification` | `SubagentStart` | `SubagentStop` | `PreCompact` | `PostCompact` | `SessionEnd` | ◐ derived |

`lost` means the agent's multiplexer session died under it. No agent reports that, because its hooks stop firing at the moment of death, so RimZ derives it from the `rimz exec` launch wrapper for every agent. Where `ended` is derived (Codex, Antigravity, Amp, Kiro), RimZ clears the card on the next snapshot tick after the pane is gone, not at the instant the agent exits.

Codex's `Interrupt` hook ends the turn with an interrupted outcome. RimZ does not install Codex's `SessionEnd` hook, because Codex also fires it when an idle app server unloads while the pane stays open.

## Per-agent mappings

Each agent's mapping doc carries its full coverage rationale, install target, resume and fork surface, and account probing, and links the upstream protocol reference. These are internals pages, written for contributors.

| Agent | Mapping | Upstream protocol |
| --- | --- | --- |
| Claude Code | [adapter_claude.md](../internals/agents/adapter_claude.md) | [claude-reference.md](../externals/agent-adapter/claude-reference.md) |
| Codex | [adapter_codex.md](../internals/agents/adapter_codex.md) | [codex-reference.md](../externals/agent-adapter/codex-reference.md) |
| Pi | [adapter_pi.md](../internals/agents/adapter_pi.md) | [pi-reference.md](../externals/agent-adapter/pi-reference.md) |
| OpenCode | [adapter_opencode.md](../internals/agents/adapter_opencode.md) | [opencode-reference.md](../externals/agent-adapter/opencode-reference.md) |
| Antigravity | [adapter_antigravity.md](../internals/agents/adapter_antigravity.md) | [antigravity-reference.md](../externals/agent-adapter/antigravity-reference.md) |
| Copilot | [adapter_copilot.md](../internals/agents/adapter_copilot.md) | [copilot-reference.md](../externals/agent-adapter/copilot-reference.md) |
| Droid | [adapter_droid.md](../internals/agents/adapter_droid.md) | [droid-reference.md](../externals/agent-adapter/droid-reference.md) |
| Cursor | [adapter_cursor.md](../internals/agents/adapter_cursor.md) | [cursor-reference.md](../externals/agent-adapter/cursor-reference.md) |
| Amp | [adapter_amp.md](../internals/agents/adapter_amp.md) | [amp-reference.md](../externals/agent-adapter/amp-reference.md) |
| Kiro | [adapter_kiro.md](../internals/agents/adapter_kiro.md) | [kiro-reference.md](../externals/agent-adapter/kiro-reference.md) |
| Qwen | [adapter_qwen.md](../internals/agents/adapter_qwen.md) | [qwen-reference.md](../externals/agent-adapter/qwen-reference.md) |
| Kimi | [adapter_kimi.md](../internals/agents/adapter_kimi.md) | [kimi-reference.md](../externals/agent-adapter/kimi-reference.md) |
| Grok | [adapter_grok.md](../internals/agents/adapter_grok.md) | [grok-reference.md](../externals/agent-adapter/grok-reference.md) |

## Agents not yet supported

An agent without an adapter runs fine in a pane, but shows as a plain process row: no card, no live state, no attention routing. A new agent gets support through one adapter over its hooks or local store ([adding an agent](../internals/agents/adapter.md#adding-an-agent)).

Two cases stay dark even for built-in agents. Remote sessions with no local pane (a `claude remote-control --spawn` worktree, or a Codex thread started from the web) are tracked but not shown. An agent whose hooks you declined at the consent prompt reports nothing until you run [`rimz hooks install`](./cli/hooks-trust.md#install-hooks).

## Third-party plugins

A process plugin connects a third-party agent through a shim that speaks RimZ's canonical event protocol. The path is under active development and not ready for outside use; what a plugin supports is declared by its bundle, not by a RimZ tier. The contract is [agent plugins](./agent-plugins.md).

## See also

- [Agents](../guide/fleet.md): launching agents and profiles.
- [Configuration](../guide/configuration.md#agent-profiles-commands-and-teams): profile fields, effort, and raw `args`.
- [Token insight](../guide/insight.md): where the `live$` and `spend` figures surface.
- [Hooks and trust](./cli/hooks-trust.md): what `rimz hooks install` writes per agent.
- [The agent model](../internals/agents/model.md): the rollup, state machine, and adapter boundary.
- [Troubleshooting](../guide/troubleshooting.md): hooks not reporting.
