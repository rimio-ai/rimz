# Agent support

RimZ has a built-in adapter for eighteen coding agents. Each adapter reads the agent's verified hooks, transcripts, or local files, and turns the available signals into the sidebar card, `rimz asks`, spend figures, and launch argv. The agent CLI runs stock, you answer it in its own UI, and the provider's web, desktop, and mobile apps keep working. This page answers, per agent: what the card shows, which files and flags RimZ touches, and where the gaps are. How the adapter boundary works is [the agent model](../internals/agents/model.md).

Each agent sits in a support tier. The tier is how much the author runs it day to day; every agent in every tier is wired against its documented surface and covered by tests. An experimental agent can still carry a wide surface, as the [compatibility matrix](#the-compatibility-matrix) shows.

| Tier | Agents | What to expect |
| --- | --- | --- |
| Supported | Claude Code, Codex | Wired end to end and run constantly. |
| Alpha | Pi, OpenCode | Close behind the supported pair. |
| Experimental | Antigravity, Copilot, Droid, Cursor, Amp, Kiro CLI, Qwen Code, Qoder, Kimi Code, Grok Build, Hermes Agent, OMP, Muse Code, MiniMax Code | Mostly works; expect the occasional bug and [report it](https://github.com/rimio-ai/rimz/issues). |

RimZ tracks each agent's latest release. Where a feature needs a minimum agent version, this page names it next to the feature.

## Check coverage on your machine

`rimz coverage` prints the capability grid with the limit behind every cell, and `rimz coverage --wiring` adds the [wiring matrix](#the-wiring-matrix) and the [lifecycle hook grid](#the-lifecycle-hook-surface). Every adapter declares its own coverage, and a test holds the declarations to the matrices on this page. The command's output, flags, and JSON shape are in [check adapter coverage](./cli/maintenance.md#check-adapter-coverage).

The terminal grids differ from this page in two ways. Cells print `✓` where this page shows ● or ✓, `!` where it shows ◐, and `✗` for ✗. Rows follow the adapter registry (claude, codex, amp, copilot, kimi, pi, opencode, antigravity, cursor, droid, kiro, qwen, grok, hermes, qoder, omp, muse, minimax) instead of tier order, and loaded [process plugins](#third-party-plugins) follow the built-ins.

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
| State | The card appears at session start, follows working, waiting, and idle, and clears when the session ends. | Every transition lands, and the card matches the pane. | The card can drift: local-store updates can lag, a cancel or failure can read as an ordinary stop, or background work can outlive a reported turn end. | No live state; the pane shows as a plain process row. |
| Live | While a turn runs: context-window fill, the token breakdown (input, output, cache), and a dollar figure. | All three, moving during the turn. | Some of it: a fill percentage without token counts, totals without the breakdown, an estimated price instead of a billed one, or figures that update only at the turn boundary. | No numbers; the card carries state only. |
| History | Past sessions readable end to end, with per-turn tokens and dollars feeding [`rimz stats`](./cli/stats.md), the provider dashboard, and the heatmap. | Transcripts, tokens, and dollars, complete across sessions. | Sessions read back, but dollars are estimated locally, incomplete, or missing. | No archive RimZ can read. |
| Account | Your login, plan, and usage windows (a 5-hour and weekly pair, monthly credits, a prepaid balance) with fill and reset time. | Plan and windows, with usage counted against them. | Identity and plan with no usage, windows with no plan, or a quota the provider publishes as display-only. | No readable account surface. |
| Ask | An agent stops and needs you: the card raises Waiting and the cockpit counts it. | The question's text and options reach [`rimz asks`](./cli/asks.md) and the card, so you can read it without opening the pane. | Only some prompts are tracked, or Waiting routes you to the pane without question detail in `rimz asks`. | A blocked agent looks like a working one. |
| Subagents | Child agents nested under the parent card. | Children appear as they start and update as they work, with name, task, and model. | Children arrive late (often when the parent's turn ends) or without part of that detail. | Children stay invisible; the parent shows a long turn. |

A few cells need more than the table says:

- State stays full for agents that publish no session-end event. Their card clears on the next sidebar refresh after the pane is gone, the same tick every card refreshes on.
- A History partial splits reading from accounting. `rimz agents logs` and `rimz agents history` still replay the session; the provider dashboard may show tokens and dollars blank or incomplete. OMP counts main-session usage but excludes task-child files.
- Answering is separate from Ask. Every agent takes its answer in its own UI, and `rimz answer` adds an out-of-band path for Claude, Codex, and Pi ([what each agent accepts](./cli/asks.md#what-each-agent-accepts)).
- Claude Code children also carry a running token count and an elapsed clock. The child row shows both for any agent that reports them.

Claude Code and Codex history also times responses for the Models `tok/s` figure in [`rimz stats`](./cli/stats.md) and [`rimz agents attribution`](./cli/agents.md#attribution). Stats scopes timing to its selected window; attribution scopes it to the report's agents. Other adapters retain their history coverage but supply no response timing.

## The compatibility matrix

One row per agent, in tier order. Run `rimz coverage` for the limit behind every ◐ and ✗.

| Agent | State | Live | History | Account | Ask | Subagents |
| --- | :--: | :--: | :--: | :--: | :--: | :--: |
| Claude Code | ● | ● | ● | ● | ● | ● |
| Codex | ● | ● | ● | ● | ● | ● |
| Pi | ● | ● | ● | ● | ● | ◐ |
| OpenCode | ● | ● | ● | ● | ● | ● |
| Antigravity | ● | ◐ | ◐ | ● | ◐ | ◐ |
| Copilot | ● | ◐ | ◐ | ◐ | ● | ◐ |
| Droid | ● | ◐ | ◐ | ✗ | ● | ✗ |
| Cursor | ● | ◐ | ◐ | ◐ | ◐ | ◐ |
| Amp | ● | ✗ | ✗ | ◐ | ◐ | ✗ |
| Kiro | ◐ | ◐ | ◐ | ✗ | ◐ | ✗ |
| Qwen | ● | ◐ | ◐ | ◐ | ● | ● |
| Qoder | ● | ✗ | ✗ | ✗ | ● | ◐ |
| Kimi | ● | ◐ | ◐ | ● | ● | ◐ |
| Grok | ● | ◐ | ● | ◐ | ● | ◐ |
| Hermes | ● | ◐ | ✗ | ✗ | ◐ | ◐ |
| OMP | ◐ | ◐ | ◐ | ◐ | ◐ | ✗ |
| Muse | ● | ✗ | ✗ | ✗ | ◐ | ✗ |
| MiniMax Code | ◐ | ◐ | ✗ | ✗ | ◐ | ✗ |

<sub>● full · ◐ partial · ✗ unsupported</sub>

A ✗ is a declared absence with a stated reason in `rimz coverage`, so a missing surface is a known gap and not a bug.

## Notes on the alpha and experimental set

These are the gaps you will notice, per agent, beyond what the matrix and `rimz coverage` state. The files `rimz hooks install` writes for each agent, and what uninstall restores, are in [what install writes](./cli/hooks-trust.md#what-install-writes). Each agent's [mapping doc](#per-agent-mappings) has the full rationale.

### Antigravity

- Permission prompts and questions stay in Antigravity's UI, because RimZ installs no permission hook. An open prompt raises Waiting and routes you to the pane.
- Context and tokens are live from the wrapped statusline. The dollar figure prices the current turn only, so it does not count toward room spend or [budget caps](./cli/budget.md).
- History replays every past session and adds no dollars to `rimz stats`.
- Children come from the parent's `invoke_subagent` transcript records, which the CLI writes late, so a fan-out often appears as the parent's turn ends. A worktree, `branch`, or `share` child stays a root card while its validated siblings nest.
- Every error stop is terminal: a supervised run does not survive a provider limit, and [auto-continue](../guide/loops.md#auto-continue) never arms.
- Yolo does not approve implementation plans (since 1.3.2). Only the user's `artifactReviewPolicy` setting lifts plan review; RimZ does not change it.

### Copilot

- With RimZ's hooks installed, launches refuse when effective `disableAllHooks` is true. Explicit ask launches also refuse a local non-manual `defaultPermissionMode`; the error names the settings file and fix. RimZ takes each key from the first file that sets it: `.github/copilot/settings.local.json`, then `.github/copilot/settings.json`, then your user `settings.json`.
- The wrapped statusline supplies the resolved model, effort, context tokens, and cumulative session tokens. The live dollar figure comes from the session's AI credits at $0.01 per credit, or is an estimate at that model when credits are absent; when the statusline is missing or replaced, RimZ falls back to OpenTelemetry metadata.
- That fallback needs a local telemetry file. An OTLP exporter configured in the environment sends the telemetry elsewhere, so RimZ warns `Copilot direct-launch enrichment is unavailable because an OTLP exporter is configured; set COPILOT_OTEL_FILE_EXPORTER_PATH to retain a file source` and the card keeps lifecycle alone. Point `COPILOT_OTEL_FILE_EXPORTER_PATH` at a file to keep both.
- History adds per-model tokens and credit-metered dollars (local estimates where credits are absent) to `rimz stats` and the provider dashboard. These are not an account billing ledger.
- Tool questions and elicitation dialogs raise Waiting. Tool questions clear when the tool completes; elicitation waits clear on subsequent lifecycle activity.
- Children appear with their model when they start, show their tool activity, and report their exact token total when they finish. A child's permission prompt arrives on the parent session, so the parent card waits. An approved shell command clears it when the child's command finishes; a denied or non-shell request clears at the parent's next hook.
- The account shows the plan and the monthly `cr`, `cht`, and `prm` windows. There are no 5-hour or weekly windows, and IDE completions, extra credits, account dollars, and remote control are unsupported.

### Cursor

- Cursor's `AskQuestion` and plan-approval ("Ready to build?") prompts have no hooks. RimZ detects an open one from Cursor's local state, raises Waiting, and routes you to the pane; `rimz asks` stays empty and `rimz answer` is unsupported.
- A later message clears a plan wait. Dismissing the plan prompt with Esc or `p` leaves the card waiting until the next turn, because Cursor records no change.
- The installed CLI accepts `subagentStart` and `subagentStop` hooks but never fires them. RimZ reads children from the chats store when the parent's next hook fires, often at turn end.
- RimZ prices each generation locally. The running session total counts toward agent and room [budgets](./cli/budget.md); provider billing, account spend, and `rimz stats` dollars are unavailable.

### Droid

- Permission and question prompts arrive as typed `Notification` hooks and reach `rimz asks`. The hook carries no request id or options, so you answer in the pane.
- A spec-mode exit shares the tool confirmation and reads as a permission. Releases without a notification type fall back to the transcript's active `AskUser` call.
- The locally priced session total reaches the card and live budgets. Provider dollars, historical spend, and quota are unavailable.
- After a compaction the card can read compacting, and messages queued for it wait, for up to 90 seconds, because Droid sends no compaction-finished hook.

### Kiro

- Turns, tool calls, and session exits arrive through the global hook file `rimz hooks install kiro` manages, which needs Kiro CLI 2.31.0 or later.
- Kiro's local session store still supplies the card before the first prompt, pending approvals, context, and history.
- A cancelled turn releases `message --wait` and `agents wait` while a sidebar runs. Errored turns still wait out the deadline; supervised `-p` runs do so on both cancel and error.
- A pending tool approval or native question raises Waiting; answer it in Kiro's pane, not through `rimz asks` or `rimz answer`.
- Live details show the model, effort, and session title; context usage is a percentage only.

### Hermes

- Hermes Agent 0.22.2 or later is required. RimZ merges its shell hooks into the top-level `hooks` key of `$HERMES_HOME/config.yaml` and launches the classic UI (`hermes chat --cli`); a `hermes --tui` session is not covered.
- Hermes asks once per hook at the first interactive launch (`Allow this hook to run? [y/N]`). Answer `y` to each RimZ hook. Until you do, `rimz doctor` lists the unconsented events, and supervised runs, loops, and message replies refuse.
- `hermes hooks doctor` reports `script missing or not executable` for `rimz hooks feed --source hermes`, as for any hook named by a bare command. Hermes still finds `rimz` on `PATH` when the hook runs.
- Approvals, `sudo` prompts, and clarify questions raise Waiting; answer them in Hermes's pane. Memory-write approvals and the secret, vault, and confirmation panels raise nothing.
- The card shows the model and the context fill of the latest model request. Cost, history, account, and launch reminders are unsupported, and `hermes-plan` launches plain Hermes.
- Named logins are refused: a separate `HERMES_HOME` does not isolate Hermes credentials.

### Kimi

- Kimi Code 2.1.1 or later is required. Installation refuses a detected older version with `kimi upgrade -y`; an unknown version does not block installation.
- Invalid hook entries block installation and preview without changing the config. Fix or remove the named entry: Kimi Code ignores every hook while one entry is invalid.
- `TurnStarted` opens main-agent background turns; user turns still open through `UserPromptSubmit` alone.
- Children appear at the next recorded root hook after their durable spawn record lands. Failed children settle as failed; cancelled and timed-out children show as finished. Finished children stay closed on resume.
- The main wire distinguishes child-fired turn ends from parent boundaries. A child's compaction still shows on the parent card; cancellation can leave it compacting until the next root signal.
- Parent live dollars, lifetime spend, and fleet spend include child wires; parent context tokens remain main-only.

### Qoder

- Launches `qodercli` 1.1.72 or newer, with an initial task passed through `-i`. Upgrade with `qodercli update` if RimZ refuses an older release.
- Permission prompts, plan approvals, and questions reach `rimz asks`; answer them in Qoder's pane. Children nest by agent type, without child tokens or transcript replay.
- Live context, tokens, dollars, history, and account quota are unavailable. Qoder's local token figures are normally hidden, and Credits have no established dollar conversion.
- A fresh worktree needs folder trust before hooks can report, and launch warns until it has it. Run `rimz trust grant --agents qoder` there before launching: answering Qoder's trust prompt in the pane saves the decision, but that launch has already lost its session-start hook, so the card stays a launch placeholder.
- Disabled hooks refuse launch, naming the settings file and fix. Use `QODER_CONFIG_DIR` for a custom home; `--config-dir`, `QODER_CLI_HOME`, and `QODER_CONFIG_DIR_NAME` refuse because the installed hooks would be in a different home.

### Qwen

- Qwen's quota is scoped to the exact provider account (region and API key), not to the CLI. A fresh supervised or loop launch reads only that account's cached Coding Plan windows: a spent window stops the launch before any pane or run record exists, and another account's window cannot stop it.
- Interactive launches, resume, fork, wait, and auto-continue ignore the quota. See [budgets: one model, five scopes](../guide/budget.md#one-model-five-scopes).

### Grok

- RimZ installs passive global hooks only; every permission decision stays in Grok's TUI.
- A running Grok session reads its hook registry as a snapshot taken at start, so `rimz hooks install` leaves that session calling whatever was registered before. Press `r` in Grok's Hooks tab, or start a new session, to pick up the installed hooks.
- Permission, plan, diff-review, and question prompts reach `rimz asks` through Grok's `Notification` hook.
- Errored and cancelled turns end through Grok's `StopFailure` and `StopCancelled` hooks, and child agents resolve from their own session hooks.
- Launch reminders reach Grok through `--rules`.
- Idle notifications rest the card only when their timestamp is newer than its last activity.
- Dollars land at each completed turn, native or locally priced. Mid-turn cost and account quota windows are unavailable.

### OMP

- OMP (Oh My Pi) is experimental, verified against the 18.11.1 prebuilt. Pi's adapter is independent.
- Tool approvals and native `ask` questions raise Waiting; question text/options reach `rimz asks`. Answer in the OMP pane, not through `rimz answer`. Plan reviews are not tracked.
- The card follows turn ends, but background work can continue afterwards. Failed manual compaction has no closing event, so the card can remain compacting until later activity.
- Context, token split, model, and thinking level update live. Dollars reconcile at turn end from main-session assistant and standalone model-usage records; task-child files and rows are excluded.
- Provider rate-limit windows appear when reported. Login identity and plan are not read, and named RimZ accounts are unsupported.
- Only the default `~/.omp/agent` home is wired. Launch preflight refuses non-default `OMP_PROFILE` (or `PI_PROFILE` when `OMP_PROFILE` is unset), nonempty `PI_CONFIG_DIR`, and nonempty `PI_CODING_AGENT_DIR`, with an unset fix. Empty, whitespace, and `default` profiles select default; an empty `OMP_PROFILE` still overrides `PI_PROFILE`.
- Raw `--no-extensions`, `--trusted-extension`, and `--profile` arguments can bypass reporting. Bun-run package installs are not detected; the prebuilt `omp` is.

### Muse

- Muse Code 1.4.9 or later is required. Install refuses an older or unreadable version and asks you to update Muse. It writes a separate managed hook file and two settings, preserving your hooks; [what install writes](./cli/hooks-trust.md#what-install-writes) names the paths and uninstall behavior.
- Turns, model tool activity, and session exits update the card. Cancellation rests the card at Idle; explicit user `!` shell actions do not start model activity. Tool approvals raise Waiting and route you to Muse's pane; questions and options stay there, and `rimz answer` is unsupported. Clarifying questions do not raise Waiting.
- Native approval timing and the empty reply's preservation of the approval UI are not live-verified. An automatically approved call may briefly raise Waiting. The failure-stop mapping is also documented-only, not captured against a real provider.
- Live context, tokens, dollars, history, account details, and child cards are unsupported. Only internal reminder children have been observed, and RimZ ignores their hooks.
- Plan selects Muse's read-only profile, but real tool-call enforcement is unverified. `/compact` was unavailable under echo; compaction lifecycle is unsupported.
- Initial prompts submit after `--`, but a prompt exactly equal to a Muse subcommand, such as `resume`, can open that command instead. Resume binds the card at the first prompt. Enterprise policy can override user managed-hook settings without the installer detecting it.

### MiniMax Code

- Launch with `minimax` or its alias `mcode`; RimZ runs the stock `mcode` CLI. Normal turns, root tool activity, and the selected model reach the card. Failed or cancelled turns leave it working until another prompt or session end.
- Root tool approvals raise Waiting and route you to MiniMax's pane. Answer there; child approvals, native questions, and plan approvals are not reported, and `rimz answer` is unsupported.
- Context gauges, token counts, dollars, conversation history, account usage, and separate native child cards are unavailable.
- RimZ adds no permission-mode flags. MiniMax keeps the mode in its `config.yaml` (default `auto`); change it with `/permission` or Alt+M. Plan Mode has no verified launch switch.
- If the plugin was disabled in MiniMax, run `mcode plugin enable rimz@local` in the same data-directory environment. RimZ checks installed files, not MiniMax's private disabled-plugin setting.
- Launch prompts starting with `-` use a separator. Prompts equal to MiniMax subcommands, such as `exec`, still dispatch those commands instead of submitting text.

## Config homes and skills

Each built-in agent resolves a config home. RimZ reads provider settings from it, installs hooks into it, and binds it into the [scoped view](../guide/configuration.md#agent-isolation). Session stores can live elsewhere, as Muse's does.

| Agent | Config home | Override | Skill root | Scoped `skills` list | Host switch |
| --- | --- | --- | --- | --- | --- |
| Claude Code | `~/.claude` | first `CLAUDE_CONFIG_DIR` entry | `<home>/skills` | accepted | `--settings skillOverrides` |
| Codex | `~/.codex` | `CODEX_HOME` | `~/.agents/skills` | accepted | `-c skills.config` |
| Pi | `~/.pi/agent` | `PI_CODING_AGENT_DIR` | `~/.agents/skills` | accepted | none |
| OpenCode | `~/.config/opencode` | `XDG_CONFIG_HOME` + `/opencode` | `~/.agents/skills` | refused | none |
| Antigravity | `~/.gemini/antigravity-cli` | none | `~/.agents/skills` | refused | none |
| Copilot | `~/.copilot` | `COPILOT_HOME` | `~/.agents/skills` | accepted | none |
| Droid | `~/.factory` | none | `~/.agents/skills` | accepted | none |
| Cursor | `~/.cursor` | `CURSOR_CONFIG_DIR`, else `XDG_CONFIG_HOME/cursor` | `~/.agents/skills` | accepted | none |
| Amp | `~/.config/amp` | `XDG_CONFIG_HOME` + `/amp` | `~/.agents/skills` | refused | none |
| Kiro | `~/.kiro` | absolute `KIRO_HOME`, excluding root and ancestors of `HOME` (including itself) | `<home>/skills` | refused | none |
| Hermes | `~/.hermes` | absolute `HERMES_HOME`, excluding root and ancestors of `HOME` (including itself) | `<home>/skills` | refused | none |
| Qwen | `~/.qwen` | `QWEN_HOME` | `<home>/skills` | accepted | none |
| Qoder | `~/.qoder` | `QODER_CONFIG_DIR` | `<home>/skills` | accepted | none |
| Kimi | `~/.kimi-code` | `KIMI_CODE_HOME` | `~/.agents/skills` | accepted | none |
| Grok | `~/.grok` | `GROK_HOME` | `~/.agents/skills` | refused | none |
| OMP | `~/.omp/agent` | none; relocated homes refused | `<home>/skills` | refused | none |
| Muse | `~/.config/muse` | `XDG_CONFIG_HOME` + `/muse` | `<home>/skills` | refused | none |
| MiniMax Code | `~/.minimax` | nonempty `MINIMAX_DATA_DIR`, then `MAVIS_DATA_DIR` | none | refused | none |
| Process plugin | none | none | none | refused | none |

Host launches link [RimZ library skills](../guide/configuration.md#skills) into the resolved skill root, including each named Claude account home, without replacing user-owned entries.

A named provider account sets the adapter's home override to a separate directory when a room launches it. MiniMax uses `MINIMAX_DATA_DIR`, relocating configuration, credentials, sessions, and plugins together; an explicit data-directory variable also disables MiniMax's Git auto-configuration. See [provider accounts](../guide/accounts.md).

Under `agents.isolation = "scoped"` (Linux), RimZ binds each agent's config home from the launch environment and merges the RimZ skill library into the agent's skill root. The scoped view does not hide the rest of the host or move credentials. Project skills and Codex's `$CODEX_HOME/skills` stay outside the merged view. A profile `skills` list keeps the listed skills model-callable and makes the rest user-only; agents marked refused in the table fail the launch when a list is set. Host Claude makes unlisted skills user-only; host Codex hides them, including explicit invocation. Other host providers warn and run unrestricted. Duplicate or invalid names are parse errors under both. The profile rules are in [configuration: skills](../guide/configuration.md#skills), and mount order and markers in [sandbox internals](../internals/sandbox.md#profile-skill-views).

## Launch flags

RimZ launches each agent with the provider's own flags. An unsupported typed `model`, `effort`, or system-prompt field fails the launch before any pane opens, naming the field. Unsupported `auto-compact` and `fast` warn and launch without that setting; invalid auto-compact counts still refuse. Raw profile `args` pass any other provider flag through unchanged, as do CLI arguments after `--`. Codex rejects `--remote X` or `--remote=X`, from either source, because managed panes run embedded with `--no-daemon`; remove `--remote` to launch. Codex requires 0.163.0 or newer for embedded sessions (`--no-daemon`) and Guardian review on every launch, resume and fork. RimZ checks the resolved launch binary before opening a pane where possible, otherwise in the exec wrapper. Headless loop and workflow judges also check the resolved binary before spawning; an unreadable version passes without a verdict.

OpenCode requires 2.0.0 or newer and runs with `--standalone` on every launch and resume. Launch, setup/install, and doctor refuse an older readable version with `opencode upgrade --major`; an unreadable version passes without a verdict. From 2.0.13, RimZ-launched sessions start with the native session tab strip off: RimZ sets `tabs.mode = "off"` over whatever `cli.json` or an exported `OPENCODE_CLI_CONFIG_CONTENT` sets and keeps every other setting. Older releases keep their tabs, and OpenCode started by hand is unchanged. Fork and model presets are unsupported. Plan adds no arguments; select the plan agent interactively with Shift+Tab. The plan-approval gate in the wiring matrix still works for those interactive turns.

Qwen requires 0.25.1 or newer. Launch refuses an older readable version with `npm install -g @qwen-code/qwen-code@latest`; an unreadable version passes without a verdict. It also refuses disabled hooks, naming the source and fix: `disableAllHooks`, `tools.executionSandbox`, `--safe-mode`, `--bare`, or truthy `QWEN_CODE_SAFE_MODE` or `QWEN_CODE_SIMPLE`. Execution-sandbox policy must be removed by its operator, or Qwen must run outside RimZ. Refresh existing Qwen hooks with `rimz hooks install` to adopt the seconds-based timeout before launching.

### Provider-session resume

`rimz agents <kind-or-profile> --resume` opens a native picker. `--resume=<id>` first checks exact ids recorded in this room; importing an unrecorded id requires a locator, which reads only the room account's home. See [provider-session resume](./cli/agents.md#resume-a-provider-session) for conflicts and refusals.

| Agent | Bare `--resume` picker | Resume by id (adopt unrecorded) |
| --- | --- | --- |
| Claude Code | `claude --resume` | Yes |
| Codex | Partial: `codex resume --no-daemon`; card binds at the first prompt | Yes, active and archived sessions |
| Pi | `pi --resume` | Unsupported |
| OpenCode | Unsupported | Unsupported |
| Antigravity | Unsupported | Unsupported |
| Copilot | `copilot --resume` | Unsupported |
| Droid | `droid --resume` | Unsupported |
| Cursor | Unsupported | Unsupported |
| Amp | Unsupported | Unsupported |
| Kiro | Partial: `kiro-cli chat --v3 --resume-picker`; card binds at the first prompt | Unsupported |
| Qwen | `qwen --resume` | Unsupported |
| Qoder | `qodercli --resume` | Unsupported |
| Kimi | `kimi --session` | Unsupported |
| Grok | Unsupported (bare native resume selects the newest session) | Unsupported |
| Hermes | Unsupported | Unsupported |
| OMP | `omp --resume` | Unsupported |
| Muse | Partial: `muse resume`; card binds at the first prompt | Unsupported |
| MiniMax Code | `mcode --session` | Unsupported |
| Process plugin | Unsupported | Unsupported |

### Permission modes

A permission mode comes from the `mode` profile field, the `--ask` and `--yolo` flags, or a `<kind>-<mode>` cell such as `claude-plan`. RimZ appends the arguments below. `none` means the mode adds no arguments, so the agent keeps its own default; for Auto and Yolo, the `<kind>-auto` and `<kind>-yolo` cells do not exist ([permission-mode cells](./cli/agents.md#permission-mode-cells)).

| Agent | Ask | Auto | Plan | Yolo |
| --- | --- | --- | --- | --- |
| Claude Code | `--permission-mode default` | `--permission-mode auto` | `--permission-mode plan` | `--dangerously-skip-permissions` |
| Codex | `--ask-for-approval on-request -c approvals_reviewer="user"` | `--ask-for-approval on-request -c approvals_reviewer="auto_review"` | none | `--dangerously-bypass-approvals-and-sandbox` |
| Pi | none | none | none | none |
| OpenCode | none | none | none | `--auto` |
| Antigravity | none | `--mode accept-edits` | `--mode plan` | `--dangerously-skip-permissions --mode accept-edits` |
| Copilot | `--mode interactive` | `--autopilot` | `--plan` | `--allow-all` |
| Droid | none | `--auto medium` | `--use-spec` | none |
| Cursor | none | `--auto-review` | `--mode=plan` | `--force --sandbox disabled` |
| Amp | none | none | none | none |
| Kiro | none | none | none | `--trust-all-tools` |
| Qwen | `--approval-mode default` | `--approval-mode auto-edit` | `--approval-mode plan` | `--approval-mode yolo` |
| Qoder | `--permission-mode default` | `--permission-mode auto` | `--permission-mode plan` | `--dangerously-skip-permissions` |
| Kimi | none | `--auto` | `--plan` | `--yolo` |
| Grok | `--permission-mode default` | `--permission-mode auto` | none | `--yolo` |
| Hermes | none | none | none | `--yolo` |
| OMP | `--approval-mode always-ask` | `--approval-mode write` | `--approval-mode always-ask` | `--approval-mode yolo` |
| Muse | `--permission-profile :ask-me` | `--permission-profile :auto-review` | `--permission-profile :read-only` | `--yolo` |
| MiniMax Code | none | none | none | none |
| Process plugin | declared by bundle | declared by bundle | declared by bundle | declared by bundle |

On host and scoped isolation, Codex keeps your sandbox and network choices. For every mode, RimZ adds only write access to the room's state, runtime, and shared runtime roots where the selected permissions support it. With legacy `workspace-write` or no explicit selection, it appends the roots to `sandbox_workspace_write.writable_roots`, preserving the array from the last argument override or the config layers. With `default_permissions = ":workspace"`, it passes `--add-dir` for each root; with a user-defined permission profile, it adds the roots through `permissions.<name>.workspace_roots`, leaving the profile's own rules in force. Read-only, full access, other built-in profiles, and profile names that are not bare TOML keys (letters, digits, `_`, `-`) receive no grant. An explicit sandbox or session config selector takes precedence over `--profile`; otherwise RimZ reads trusted project `.codex/config.toml` layers over `$CODEX_HOME/<name>.config.toml` over base config. It does not read system config layers or managed requirements. A profile needing full access can pass `--sandbox danger-full-access` in its args.

Under `agents.isolation = "sandbox"`, Codex receives `--sandbox workspace-write` only when the selection above finds no sandbox mode in its arguments or config. Any selected mode, including full access, a permission profile, or Yolo, stays as selected. The room-root grant then follows the resulting selection. Claude's merged settings add `sandbox.enabled: true`. Every other provider warns and launches without a sandbox flag ([security guide](../guide/security.md#provider-sandbox-isolation)).

Interactive Ask uses the human reviewer; Auto uses Guardian, including headless runs. Headless Ask cannot escalate, because Codex forces approval policy `never` for the human reviewer. Yolo still bypasses both sandbox and approval checks. Codex validates the user's permissions at startup.

Claude and Codex additionally receive routine-command permissions by default, independent of permission mode. Claude gets one `Bash(<prefix> *)` rule per prefix through a merged `--settings`, matching the bare command and its arguments. Codex gets one `prefix_rule` per prefix in the installed `$CODEX_HOME/rules/rimz.rules`:

- `rimz message`, `rimz agents`, `rimz subagents`, `rimz teams`
- `rimz asks`, `rimz answer`, `rimz wait --in`
- `rimz pane list`, `rimz pane capture`, `rimz loop show`, `rimz loop logs`
- `rimz lsp def`, `rimz lsp refs`, `rimz lsp hover`, `rimz lsp impl`, `rimz lsp callers`, `rimz lsp callees`, `rimz lsp symbols`, `rimz lsp find`, `rimz lsp check`, `rimz lsp list`, `rimz lsp status`

There is no blanket RimZ grant. `pane send`, `wait --run`, `loop add`, `events emit`, and `lsp shim`, `attach`, or `stop` are not added. Claude's settings also union the agent-visible scratch/shared directories into `permissions.additionalDirectories` and add an `autoMode.environment` entry retaining `$defaults`; profile entries remain. Opt out with machine-only [`[agents] allow-routine-rimz = false`](../guide/configuration.md#routine-rimz-commands). Independently of that switch, a launch gets the canonical archive directory from the nearest working `.rimz/team` link between its cwd and checkout root (or any ancestor outside Git), including subagents launched from checkout subdirectories. A team-tagged launch gets it only when the manifest names that team; an untagged child accepts any working run. A mismatched nearest run grants nothing, not the root's run instead. The grant never covers the whole `team-notes/` class. These are Claude permission settings, not permission hooks; Claude's managed policy and explicit ask/deny rules still apply. The `rimz agents` and `rimz subagents` rules also allow launches with `--yolo` or a passthrough bypass flag. The `rimz teams` rule covers the whole scene: launching, stopping, and restarting cohorts, resuming one with `--yolo`, and `rimz teams install`, which writes team and agent definitions from the RimZ repository into `~/.rimz/` and replaces existing ones with `--force`. `rimz answer` allows answering another agent's prompt ([security](../guide/security.md#what-can-run-a-command)).

Claude's same `--settings` adds exact `Skill(<name>)` rules for every profile-listed skill, independent of that switch, and for installed `rimz-*` skills when the switch is on. Names are directory names, not frontmatter names, from the provider skill root or RimZ library; names containing `*` receive no rule. Profile rules stay first, and additions are deduplicated.

Markdown definitions can also supply [`allowed-tools`](definitions.md#tools-to-provider-arguments) permission rules. Claude merges them into the same `permissions.allow`, in all isolation levels and independently of the routine switch. Other kinds warn in `agents validate` and at launch and do not apply the list. Claude auto mode drops some broad rules; the definition reference describes that limit.

Codex's managed `rules/rimz.rules` carries a first-line marker and is rewritten by `rimz hooks install`. Existing installs get a rules-file preview at the next `rimz start` consent. Matching commands run unsandboxed, without Guardian or a permission hook; shell redirects do not match the inner prefix. Explicit prompt/forbidden rules and managed policy still apply ([upstream rule contract](../externals/agent-adapter/codex-reference.md#execution-policy-rules)). Set `allow-routine-rimz = false` to remove the managed file at the next hook install or start consent; an unmarked user file is left alone even during an explicit install, and your own rules still apply. The `rules` directory is shared across named accounts, including standalone accounts and Codex's own `default.rules` amendments. With the switch off, commands outside the three granted room roots need escalation, including `rimz teams install` and `rimz wait --in`. The latter can insert its durable wait before failing on the home-wide loops lock, without rolling the insert back.

Under `agents.isolation = "scoped"`, Codex keeps the same sandbox, network, and approval choices as under host isolation, including room-root grants. Claude's merged settings grant its sandbox room-root writes at either level and exclude the routine prefixes above when enabled; scoped isolation additionally permits nesting. Muse alone receives `--disable-sandbox` under scoped isolation ([sandbox internals](../internals/sandbox.md#provider-command-sandboxes)).

Grok Plan adds no arguments because Grok's `/plan` is an interactive command, and a session launched with `--permission-mode plan` does not start in plan mode.

OMP Plan uses the same conservative approval flags as Ask, not native plan mode. OMP's `--plan` selects a model role; launching OMP without a permission mode keeps its native yolo default.

### Model and effort

`--model` and `--effort` on `rimz agents`, and the `model` and `effort` profile fields, render as below. RimZ passes the value through without checking it, so effort levels are whatever the agent's CLI accepts.

| Agent | Model | Effort |
| --- | --- | --- |
| Claude Code | `--model <model>` | `--effort <level>` |
| Codex | `--model <model>` | `-c model_reasoning_effort=<level>` |
| Pi | `--model <model>` | `--thinking <level>` |
| OpenCode | refused | refused |
| Antigravity | `--model <model>` | `--effort <level>` |
| Copilot | `--model <model>` | `--reasoning-effort <level>` |
| Droid | refused | refused |
| Cursor | `--model <model>` | refused |
| Amp | `--mode <model>` | refused |
| Kiro | `--model <model>` | `--effort <level>` |
| Qwen | `--model <model>` | refused |
| Qoder | `--model <model>` | `--reasoning-effort <effort>` |
| Kimi | `--model <model>` | refused |
| Grok | `--model <model>` | `--reasoning-effort <level>` |
| Hermes | `--model <model>` | `--reasoning <level>`; levels other than `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, and `ultra` are refused |
| OMP | `--model <model>` | `--thinking <level>` |
| Muse | `--model <model>` | `--reasoning-effort <level>` |
| MiniMax Code | `--model <provider/model[#variant]>` | refused |
| Process plugin | declared by bundle | declared by bundle |

Amp has no model flag; its `--mode` selects the agent mode, and the `model` value is passed there. The `--max-turns` cap for supervised runs is in [supervised runs](./cli/agents.md#supervised-runs--p).

### Fast mode

The `fast` profile and role field asks the provider for its fast mode (priority serving, at a higher price). Fast is on or off. On renders the switch below; off renders nothing, so the provider's own default stands (a saved Claude `/fast`, a Codex `service_tier` in `config.toml`). The launch record keeps the effective value, so restart, fork, and resume replay it like the recorded model.

| Agent | Switch |
| --- | --- |
| Claude Code | `"fastMode": true` inside the launch's one `--settings` |
| Codex | `-c service_tier=fast -c features.fast_mode=true` |
| Amp | `--fast` |
| Cursor | skipped with a warning: choose a fast model ID with `--model` (for example `claude-opus-5-5-fast`) |
| Kimi | skipped with a warning: use `--model kimi-for-coding-highspeed` |
| Droid | skipped with a warning: toggle `/fast` inside the session |
| Every other built-in and process plugin | skipped with a warning |

A definition that sets `fast: true` on a kind without a switch loads normally; each launch warns with the alternative and records fast as off. The same rule applies to role fields, `--fast`, fast cells, and loop tasks. Claude merges `fastMode` into the same settings value as RimZ's other settings, since only the last `--settings` takes effect. Codex also turns on its `fast_mode` feature, since a user-level `fast_mode = false` otherwise drops the tier silently; a managed feature pin still wins without an error. Amp fixes fast when a thread is created. Current Amp builds leave RimZ no local token record, so no Amp turn is priced.

The fast-mode badge follows the provider's live reading: Claude's statusline `fast_mode`, or Codex's latest `thread_settings_applied` service tier. An explicit live off reading clears the badge even after a fast launch. Without a live reading, including Amp, RimZ falls back to the launch record; a launch with fast off cannot rule out the provider's saved default. Droid has no live reading or launch switch, and Cursor's fast models already name their speed.

### Launch-prompt replacement

The profile fields `system-prompt-file` and `append-system-prompt-files` (and the matching `rimz agents` flags) replace the agent's system prompt. RimZ joins the base file and the appended files in order into one prompt, then hands it to the agent.

| Agent | Replacement | How the agent receives it |
| --- | :--: | --- |
| Claude Code | ✓ | `--system-prompt-file <path>` |
| Codex | ✓ | `-c model_instructions_file=<path>` |
| Pi | ✓ | the prompt text in `--system-prompt`, at most 120 KiB |
| OMP | ✓ | the prompt text in `--system-prompt`, at most 120 KiB |
| Qwen | ✓ | the composed file's path in `QWEN_SYSTEM_MD` |
| Droid | ✗ | Droid only appends; pass its flag through profile `args` |
| MiniMax Code | ✗ | launch reminders append through `--append-system-prompt`; native replacement flags require raw `args` |
| OpenCode, Antigravity, Copilot, Cursor, Amp, Kiro, Kimi, Grok, Qoder, Muse | ✗ | no verified replacement flag |
| Hermes | ✗ | its only system-text input, `HERMES_EPHEMERAL_SYSTEM_PROMPT`, discards the user's personality and `agent.system_prompt` |
| Process plugin | declared by bundle | the composed file's path in `[launch].system-prompt-file-flag` |

Pi's replacement is not complete: Pi still appends its own `APPEND_SYSTEM.md`, context files, skills, and working-directory material after RimZ's text. The full prompt is visible in Pi's process arguments.

OMP likewise retains its discovered append/context material after replacement, and the full replacement text is visible in process arguments. RimZ's launch reminders append through the extension without displacing `APPEND_SYSTEM.md`.

### Interrupt key

[`rimz message --interrupt`](./cli/message.md#when-a-message-lands) types a prompt, then presses the agent's interrupt key to stop the live turn. The message reference describes when Codex replays the prompt as a fresh turn. Only agents with a declared interrupt key support this mode.

| Agent | Interrupt key | Unsupported behaviour |
| --- | :--: | --- |
| Claude Code | Esc | Supported, without stop confirmation |
| Codex | Esc | Supported; the Interrupt hook confirms the stop, otherwise RimZ warns |
| Every other agent and process plugins | none | `--interrupt` refused; use `--steer` or park |

### Auto-compaction window

The `auto-compact` field on a profile or team role sets the agent's native auto-compaction window, as a token count from 100k through 1M inclusive: `"200k"`, `"200000"`, or `"1m"`. RimZ rejects anything outside that range before launch, so `"200"` fails instead of meaning something different to each CLI. This is separate from [RimZ smart compaction](../guide/configuration.md#smart-compaction). Which agents accept a trailing instruction when you compact by hand is in [`rimz agents compact`](./cli/agents.md#compact).

| Agent | Auto-compaction window | How the agent receives it |
| --- | :--: | --- |
| Claude Code | ✓ | `--autocompact <tokens>`; needs Claude Code 2.1.221 or later, and Claude caps it at the model's context window |
| Codex | ✓ | `-c model_auto_compact_token_limit=<tokens>`; Codex clamps it to 90% of the model window |
| Every other agent and process plugins | ✗ | skipped with a warning at launch |

Profiles without an explicit or inherited `auto-compact` use the machine [`[harness] auto_compact`](../guide/configuration.md#native-auto-compaction) default, `272k`, on Claude and Codex. This covers Markdown definitions and trusted project TOML profiles; bare-kind launches are unchanged. A switch to a provider without a window flag silently drops this injected default; a declared value warns instead. `"off"` passes no native window, leaving the provider's own default. The clamps above still apply.

## The wiring matrix

The wiring matrix is the mechanism under the six capabilities: nineteen integration concerns, each naming one thing an adapter reads from or drives in its agent. Use it to find why a capability reads partial, or when [building an adapter](../contributing/agent-adapters.md).

| Agent | `turn` | `perm` | `plan` | `ask` | `answer` | `compact` | `sub` | `remind` | `bg` | `bgsh` | `end` | `idle` | `usage` | `live$` | `rich` | `install` | `spend` | `tools` | `remote` |
| --- | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: | :--: |
| Claude | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Codex | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✗ | ✗ | ◐ | ◐ | ✓ | ✓ | ✓ | ✓ | ✓ | ◐ | ✓ |
| Pi | ✓ | ✗ | ✗ | ✓ | ✓ | ✓ | ✓ | ✓ | ✗ | ✗ | ✓ | ◐ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✗ |
| OpenCode | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ | ✓ | ✓ | ✗ | ✗ | ✓ | ◐ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✗ |
| Antigravity | ✓ | ◐ | ✗ | ◐ | ✗ | ✗ | ◐ | ✗ | ✓ | ✗ | ◐ | ◐ | ✓ | ◐ | ✓ | ✓ | ✗ | ◐ | ✗ |
| Copilot | ✓ | ✓ | ✗ | ✓ | ✗ | ◐ | ◐ | ✗ | ✗ | ✗ | ✓ | ◐ | ✓ | ◐ | ✓ | ✓ | ◐ | ✗ | ✗ |
| Droid | ✓ | ✓ | ◐ | ✓ | ✗ | ◐ | ✗ | ✓ | ✗ | ✗ | ✓ | ◐ | ✓ | ◐ | ◐ | ✓ | ✗ | ✗ | ✗ |
| Cursor | ✓ | ✗ | ◐ | ◐ | ✗ | ◐ | ◐ | ✗ | ✗ | ✗ | ✓ | ◐ | ✓ | ◐ | ✓ | ✓ | ✗ | ✗ | ✗ |
| Amp | ✓ | ✓ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ◐ | ◐ | ✗ | ✗ | ✗ | ✓ | ✗ | ✗ | ✗ |
| Kiro | ✓ | ◐ | ✗ | ◐ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✓ | ◐ | ◐ | ✗ | ◐ | ✓ | ✗ | ✗ | ✗ |
| Qwen | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ | ✓ | ✓ | ◐ | ✓ | ✓ | ◐ | ✗ | ✗ |
| Qoder | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ | ✓ | ✓ | ✗ | ✗ | ✓ | ✓ | ✗ | ✗ | ✗ | ✓ | ✗ | ✗ | ✗ |
| Kimi | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ | ◐ | ✗ | ✗ | ✗ | ✓ | ◐ | ◐ | ✓ | ✗ | ✓ | ◐ | ✗ | ✗ |
| Grok | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ | ◐ | ✓ | ✗ | ✗ | ✓ | ✓ | ✓ | ◐ | ◐ | ✓ | ✓ | ✗ | ✗ |
| Hermes | ✓ | ◐ | ✗ | ◐ | ✗ | ✗ | ◐ | ✗ | ✗ | ✗ | ◐ | ◐ | ✓ | ✗ | ◐ | ✓ | ✗ | ✗ | ✗ |
| OMP | ✓ | ✓ | ✗ | ✓ | ✗ | ✓ | ✗ | ✓ | ✗ | ✗ | ✓ | ◐ | ✓ | ◐ | ✓ | ✓ | ◐ | ✓ | ✗ |
| Muse | ✓ | ✓ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✓ | ✗ | ✗ | ✗ | ✗ | ✓ | ✗ | ✗ | ✗ |
| MiniMax Code | ✓ | ✓ | ✗ | ✗ | ✗ | ✓ | ✗ | ✓ | ✗ | ✗ | ✓ | ◐ | ✗ | ✗ | ◐ | ✓ | ✗ | ✓ | ✗ |

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
| `bgsh` | Background shells listed in the card's waits section while they run. |
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
| Pi | `session_start` | `before_agent_start` | `agent_settled` (`agent_end` before Pi 0.80.4) | `tool_execution_end` | `tool_call`; `ui_prompt_start` (Pi 0.84.4+) | `subagent_started` | `subagent_stopped` | `session_before_compact` | `session_compact`; `session_compact_failed` | `session_shutdown` | ◐ derived |
| OpenCode | `session_created` | `chat_message` | `session_idle` | `tool_after` | `permission_ask`; `session_idle` + plan turn | `SubagentStart` | `SubagentStop` | `session_compacting` | `session_compacted` | `session_ended` | ◐ derived |
| Antigravity | ◐ first `PreInvocation` + local discovery | `PreInvocation` | `Stop` | `PostToolUse` | ◐ statusline permission marker + transcript question | ◐ child `PreInvocation` + parent transcript | ◐ child `Stop` + parent transcript | ✗ | ✗ | ◐ derived | ◐ derived |
| Copilot | `sessionStart` | `userPromptSubmitted` | `agentStop` | `postToolUse` | `permissionRequest` | ◐ child `userPromptSubmitted` + parent transcript | ◐ child `agentStop` + parent transcript | `preCompact` | ◐ derived | `sessionEnd` | ◐ derived |
| Droid | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `Notification` | ✗ | ✗ | `PreCompact` | ◐ derived | `SessionEnd` | ◐ derived |
| Cursor | `sessionStart` | `beforeSubmitPrompt` | `stop` | `postToolUse` | ◐ local pending `AskQuestion` or plan proposal | ◐ chat-store child | ◐ child transcript | `preCompact` | ◐ derived | `sessionEnd` | ◐ derived |
| Amp | `session_start` | `agent_start` | `agent_end` | `tool_result` | `permission_ask` | ✗ | ✗ | ✗ | ✗ | ◐ derived | ◐ derived |
| Kiro | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | ◐ pending interaction | ✗ | ✗ | ✗ | ✗ | `SessionEnd` | ◐ derived |
| Qwen | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `PermissionRequest` | `SubagentStart` | `SubagentStop` | `PreCompact` | `PostCompact` | `SessionEnd` | ◐ derived |
| Qoder | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `PermissionRequest` | `SubagentStart` | `SubagentStop` | `PreCompact` | `PostCompact` | `SessionEnd` | ◐ derived |
| Kimi | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `PermissionRequest` | ◐ durable `subagent.spawned` records | ◐ durable subagent terminal records | `PreCompact` | `PostCompact` | `SessionEnd` | ◐ derived |
| Grok | `SessionStart` | `UserPromptSubmit` | `Stop` / `StopFailure` / `StopCancelled` | `PostToolUse` | `Notification` | `SubagentStart` | ◐ child turn report + correlation | `PreCompact` | `PostCompact` | `SessionEnd` | ◐ derived |
| Hermes | `on_session_start`; `on_session_reset` | `pre_llm_call` | `on_session_end` | `post_tool_call` | ◐ `on_human_input_request` | `subagent_start` | `subagent_stop` | ✗ | ✗ | ◐ derived | ◐ derived |
| OMP | `session_start` | `before_agent_start` | `agent_end` without `willContinue` | `tool_execution_end` | `tool_call`; `tool_approval_requested` | ✗ | ✗ | `session_before_compact` | `session_compact`; failed `auto_compaction_end` | `session_shutdown` | ◐ derived |
| Muse | `SessionStart` | `UserPromptSubmit` | `Stop` / `StopFailure` / `Interrupt` (interrupted) | Model `PreToolUse` / `PostToolUse` / `PostToolUseFailure` | `PermissionRequest` | ✗ | ✗ | ✗ | ✗ | `SessionEnd` | ◐ derived |
| MiniMax Code | `SessionStart` | `UserPromptSubmit` | `Stop` | `PostToolUse` | `PermissionRequest` | ✗ | ✗ | `PreCompact` | `PostCompact` | `SessionEnd` | ◐ derived |

`lost` means the agent's multiplexer session died under it. No agent reports that, because its hooks stop firing at the moment of death, so RimZ derives it from the `rimz exec` launch wrapper for every agent. Where `ended` is derived (Codex, Antigravity, Amp, Hermes), RimZ clears the card on the next snapshot tick after the pane is gone, not at the instant the agent exits.

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
| Qoder | [adapter_qoder.md](../internals/agents/adapter_qoder.md) | [qoder-reference.md](../externals/agent-adapter/qoder-reference.md) |
| Kimi | [adapter_kimi.md](../internals/agents/adapter_kimi.md) | [kimi-reference.md](../externals/agent-adapter/kimi-reference.md) |
| Grok | [adapter_grok.md](../internals/agents/adapter_grok.md) | [grok-reference.md](../externals/agent-adapter/grok-reference.md) |
| Hermes | [adapter_hermes.md](../internals/agents/adapter_hermes.md) | [hermes-reference.md](../externals/agent-adapter/hermes-reference.md) |
| OMP | [adapter_omp.md](../internals/agents/adapter_omp.md) | [omp-reference.md](../externals/agent-adapter/omp-reference.md) |
| Muse | [adapter_muse.md](../internals/agents/adapter_muse.md) | [muse-reference.md](../externals/agent-adapter/muse-reference.md) |
| MiniMax Code | [adapter_minimax.md](../internals/agents/adapter_minimax.md) | [minimax-reference.md](../externals/agent-adapter/minimax-reference.md) |

## Agents not yet supported

An agent without an adapter runs fine in a pane, but shows as a plain process row: no card, no live state, no attention routing. A new agent gets support through one adapter over its hooks or local store ([adding an agent](../internals/agents/adapter.md#adding-an-agent)).

Two cases stay dark even for built-in agents. Remote sessions with no local pane (a `claude remote-control --spawn` worktree, or a Codex thread started from the web) are tracked but not shown. An agent whose hooks you declined at the consent prompt reports nothing until you run [`rimz hooks install`](./cli/hooks-trust.md#install-hooks).

## Third-party plugins

A process plugin connects a third-party agent through a shim that speaks RimZ's canonical event protocol. The path is under active development and not ready for outside use; what a plugin supports is declared by its bundle, not by a RimZ tier. The contract is [agent plugins](./agent-plugins.md).

## See also

- [Agents](../guide/fleet.md): launching agents and profiles.
- [Configuration](../guide/configuration.md#agent-profiles-commands-and-teams): where profiles, teams, and the skill library live on disk.
- [Token insight](../guide/insight.md): where the `live$` and `spend` figures surface.
- [Hooks and trust](./cli/hooks-trust.md): what `rimz hooks install` writes per agent.
- [The agent model](../internals/agents/model.md): the rollup, state machine, and adapter boundary.
- [Troubleshooting](../guide/troubleshooting.md): hooks not reporting.
