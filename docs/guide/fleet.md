# Agents

RimZ watches the agents you already run. Type `claude` into any pane and it joins the room; `rimz agents` earns its keystrokes later, when you want an agent tuned for one job or several agents launched in one line. This page covers both ways in and the commands that read and drive the fleet once it is working; the field-level detail behind profiles and permission modes waits at the [end of the page](#profiles-shape-an-agent-for-one-job).

## Run the CLI you already run

Type the agent's own command in any pane, exactly as you did before RimZ:

```sh
claude          # the stock Claude CLI
codex           # the stock Codex CLI
```

The agent appears in the sidebar, reporting from its first line. No `rimz` command sits in the path: the CLI runs with your flags, your config, your session files, and the reporting hooks you approved when you [installed hooks](./setup.md#install-agent-hooks) tell RimZ what it does — status, task, context health, live cost. From that the agent gets a live card, a handle you can message, and a place in the attention ranking. Which agents RimZ drives, and what each integration reports, is [agent support](../reference/agent-support.md).

For a single agent in the pane you are standing in, this is the whole story. Everything below is for the sessions where it isn't.

## Why `rimz agents`

A bare `claude` is a general-purpose agent, and most work is not general: a planner should reason hard and edit nothing, a reviewer should read the diff and propose rather than commit, a test-writer should stay in the test tree. Even the everyday default is worth shaping — a trimmed system prompt, an explicit tool list, the effort and permission posture you actually want — and the stock CLI takes all of it through its own flags:

```sh
claude --permission-mode auto --effort xhigh \
       --system-prompt-file ~/.agents/prompts/claude-slim.md \
       --strict-mcp-config \
       --tools 'Agent,AskUserQuestion,Bash,Edit,EnterPlanMode,ExitPlanMode,LSP,Read,Skill,TaskCreate,TaskGet,TaskList,TaskStop,TaskUpdate,WebFetch,WebSearch,Write'
```

A shaped agent beats a general one steered by hand — fewer wrong turns, less context spent wandering, a lower bill for the same result. What doesn't scale is the typing: that flag stack, re-entered in every pane, kept in sync across sessions, remembered per provider because Codex spells the same ideas differently.

That flag stack is what `rimz agents` bottles. A [profile](#profiles-shape-an-agent-for-one-job) — here named `claude`, the same as the kind, so it becomes the tuned default — gives it one word, and the launcher replays it:

```sh
rimz agents claude            # the flag stack above, as one word — and a @claude handle
rimz agents claude,codex      # two agents, side by side in one line
rimz agents forge -w feat-x   # a whole team, isolated in its own worktree
```

The wrapper stays thin. `rimz agents claude` does exactly two things on your machine: it renders the profile into the stock CLI's own flags (the `claude --permission-mode auto …` line above, nothing you couldn't type yourself), and it runs that command in your Zellij or tmux — in the pane you are standing in for a single agent, in a fresh tab for a layout or worktree — under a small RimZ launcher that stamps the handle and hands over to the CLI. The agent process is the official CLI; its session files land where the CLI always puts them, so `claude --resume` and the provider's own apps keep working. Closing the pane or `rimz agents stop @claude` ends it the same way Ctrl+C would.

Beyond the preset, the launcher carries three habits that build on it, each with its own guide:

- **several agents at once**, arranged in a [layout](#compose-a-layout) from one spec,
- **an isolated [worktree](./worktrees.md)** per line of work, one `-w` flag away,
- **a named [team](./teams.md)** of profiles, launched, messaged, and resumed as a unit.

## Compose a layout

Launching three agents by hand is three pane splits and three commands typed. One spec does it in one line: **commas split columns, plus signs tile rows, slashes stack rows** (a Zellij stack; tmux tiles them). Each cell is an agent kind, a `<kind>-<mode>` cell ([permission modes](#set-a-permission-mode)), a [profile](#profiles-shape-an-agent-for-one-job), a configured command, or `term` for a plain shell; suffix an agent cell with `:role` to give it an ad-hoc role handle. An optional trailing prompt goes to one leader: a named team's configured `leader` role, its first declared role by default, or otherwise the first agent cell. Give a repeated first cell an inline role to make that target unambiguous; use `rimz message @all` after launch when every agent needs the same text.

```sh
rimz agents claude,codex                     # two agents, side by side
rimz agents claude:planner,codex:coder -w feat-x   # ad-hoc role handles, no agents.toml
rimz agents claude,codex+term                # Claude | Codex tiled over a shell
rimz agents claude/codex/term                # one stack of three rows
rimz agents 'vim,codex+term'                 # your editor beside an agent stacked over a shell
rimz agents claude,codex "Draft the API shape."   # the prompt lands on the first cell, Claude
```

Quote a spec whenever it contains a `+`, a space, or anything your shell would otherwise expand. Profiles and kinds compose the same way, so `rimz agents planner,coder+reviewer` lays out three of your presets. The full grammar and how cells compile to panes is [harness.md → The layout IR](../internals/harness/harness.md#the-layout-ir).

Add `-w` and the whole layout lands in an isolated Git worktree, the pattern for running several agents in parallel without stepping on each other: see [Worktrees](./worktrees.md).

## Handles, in brief

Every agent answers to a handle: `@claude` names a kind, `@planner` names a profile or an inline `cell:planner` role, and `forge.reviewer` names one role of a team. RimZ gives each bare launch a stable pet name too (`@swift-otter`), and `--name writer` pins your own (`@writer`). A `#channel` suffix scopes the handle to one channel — every worktree gets one, and named channels and teams have their own — defaulting to the one you are standing in.

```sh
rimz agents focus @claude-2#feat-a   # jump to a specific agent's pane
rimz agents show swift-otter         # its activity, context, placement, and transcript tail
```

That is enough to launch, re-add, and jump to agents. Routing text to them — parking at the turn boundary, steering the live turn, broadcasting to a channel — is the [Messaging guide](./messaging.md), which owns the full address grammar and delivery rules.

## Manage a running room

Once a few agents are working, the same `rimz agents` command reads the room and drives it. Every verb below takes a [handle](#handles-in-brief), so `@coder` is the codex you launched into a team and `@swift-otter` the bare Claude you started in the corner. These commands run from any pane in the room, or from a script anywhere that resolves to the same workspace.

**See the whole room at a glance.** Bare `rimz agents` lists the current channel's cards, grouped by the worktree each lives in and ordered so whoever needs you sits on top:

```console
$ rimz agents
AGENT         STATUS   MODEL         CTX  TOKENS  AGE

⑂ auth-refresh · forge team
@planner      waiting  opus@high      42%     78k   2m
  which rotation strategy should we use?

@coder        running  gpt-5.5@high   31%     54k   0s
  wire up the refresh-token path

@reviewer     idle     opus@high       3%     12k  15m
  review the diff once coder lands

query-engine
@swift-otter  success  opus@high      78%    120k   8m
  store refactor
```

`@planner` sits first because it stopped to ask you something; the columns read its status, model and effort, how full its context window is, tokens used, and how long since it last moved, while the indented line below says what it is on. Add `--all` to widen past the current channel to every lane in the room, or a scope like `rimz agents '#auth-refresh'` to read one.

**Ask why one agent is where it is.** When a card raises a question you can't answer from one line, `rimz agents show` prints the full report for a single agent, so you see what you asked it, what it is spending, and where its pane lives without switching to it:

```console
$ rimz agents show @coder
Agent
  handle:  @coder#auth-refresh
  kind:    codex
  role:    coder
  team:    forge
  session: 01JQZ8L4M2P9RT

Activity
  description:   wire up the refresh-token path
  status:        running
  phase:         acting
  turn_started:  2026-07-08T15:41:02Z
  turn_elapsed:  3m
  last_activity: 0s

Context
  model:               gpt-5.5@high
  fill:                31%
  window:              272000
  total_tokens:        54210
  fresh_input_tokens:  4180
  cache_read_tokens:   47600
  cache_write_tokens:  1920
  output_tokens:       510
  compactions:         0
  cost:                $0.87

Placement
  channel:  auth-refresh
  worktree: ~/code/query-engine-auth-refresh
  pane:     tmux:%14
  tab:      auth-refresh

Recent transcript
you  15:38
  after your turn, add coverage for the expiry edge cases

coder  15:41
  Wiring the rotation path first, then the expiry tests.
```

Add `--capture` to append the pane's visible text, or `--json` to hand the same report to a script.

**Read along without leaving your pane.** `rimz agents logs` tails an agent's transcript; `-f` follows new lines as the turn writes them, so you watch a long run from the pane you are already in:

```console
$ rimz agents logs @coder -n 4
coder  15:36
  Rotating the refresh token first, then wiring the retry path.

you  15:38
  after your turn, add coverage for the expiry edge cases

coder  15:41
  Added tests/auth/refresh_expiry.rs covering sliding and hard expiry.

$ rimz agents logs @coder -f    # follow new lines as they land
```

**Read what each turn cost.** `rimz agents history` joins the conversation's user turns to the provider's token and price records, so you can see which request consumed the session without leaving the agent view. `-n` keeps the newest rows and `--json` returns the same records to a script:

```console
$ rimz agents history @coder -n 3
START             DUR  TOKENS       COST     OUTCOME  PROMPT
2026-07-08 15:12   8m  ↘4k ↗510     $0.4210  done     implement refresh-token rotation
2026-07-08 15:38   3m  ↘1k ↗284     $0.2870  done     add coverage for expiry edge cases
2026-07-08 15:44  12s  ↘320 ↗0      $0.0310  open     run the focused integration tests
3 turns · 54k tokens · $0.7390
```

**Find what is burning CPU or tokens.** When the machine gets loud, `rimz agents top` ranks the live fleet by the resources each agent's pane process tree is using. It streams by default; `--once` takes a sample and exits for a script:

```console
$ rimz agents top --once
4 agents · 2 running · 143% CPU · 1.9G MEM · 248k tokens
AGENT         STATUS   CPU   MEM  IO/S  PROCS  CTX  TOKENS  AGE
@coder        running  96%  892M  4M/s     12  31%     54k  0s
@planner      waiting   2%  410M     -      6  42%     78k  2m
@reviewer     idle      1%  180M     -      4   3%     12k  15m
@swift-otter  success    -     -     -      -  78%    120k  8m
```

**Jump in, or shut one down.** A row you want to answer is one `focus` away; a pane you are done with is one `stop`:

```sh
rimz agents focus @coder        # jump to its pane
rimz agents stop @reviewer      # close the idle reviewer's pane
rimz agents stop @claude --all  # close every Claude in scope
```

`stop` closes the agent's pane, ending the CLI process the way Ctrl+C would; sessions stay on disk in the provider's own format, so a stopped agent is one `--resume` away.

**Restore a lane by place.** `rimz agents resume '#auth-refresh'` focuses the lane when every member is live, adds only closed members when part of it remains live, and rebuilds the saved team and stray panes when all are closed. Use `--from-pr 42` for a locally developed pull-request lane; bare `resume` targets the current worktree or lists resumable lanes at the project root.

**Bounce an agent in place.** `rimz agents restart @coder` focuses the agent, replaces its pane in the same layout position, and resumes the provider session with the original profile, role, team, channel, and permission mode. The profile is rendered from the current `agents.toml`, so edits take effect on the bounce. When the provider has no resumable conversation, restart launches fresh and prints the allocated replacement handle instead of hiding a possible rename.

**Fork an agent to try another approach.** `rimz agents fork @coder` takes over the launching pane with the full conversation under a new provider-assigned session id in the source worktree, leaving the original session untouched and preserving its permission mode. Pass `--new-pane` to keep the launching pane, or `--new-tab` for a separate view. RimZ gives the fork a fresh pet name; use `rimz agents fork @coder --name twin` to pin `@twin` when you want both approaches to have memorable handles.

Two everyday tasks have their own guides, with the depth this page leaves out:

- **Steer or queue an agent** — send text that parks at the turn boundary, interrupts the live turn, or arrives on a schedule: the [messaging guide](./messaging.md).
- **Script an agent** — run one supervised, exit-coded turn for a pipeline or CI job with `rimz agents … -p`: the [scripting guide](./scripting.md).

The complete `rimz agents` surface, every verb and flag, is the [agent-control reference](../reference/cli/agents.md).

## Answer asks from your phone

An agent that stops to ask while you are away does not have to wait for your terminal. Claude Code and Codex ship **remote control**, the bridge behind their official mobile apps, and two toggles keep it up with every room:

```sh
rimz config set remote_control.claude true
rimz config set remote_control.codex true
```

The ask reaches your phone as a push from the provider's own app, and your answer lands in the same session as if you had typed it in the pane. Both toggles are off by default; exactly what each one runs, the preconditions, and the undo are in [remote → answer asks from your phone](./remote.md#answer-asks-from-your-phone).

That is the whole daily workflow. The two sections below are the detail it names — the profile fields and the permission-mode suffixes — here for when you need a specific field rather than on your way in.

## Profiles: shape an agent for one job

A **profile** is a named preset in `agents.toml`: the base CLI plus the fields that shape it — model, reasoning effort, system prompt, permission mode, and raw flags. Define it once, launch it by name:

```toml
[agents.profiles.claude]                                   # named for the kind, so it becomes the tuned default
agent = "claude"                                           # the base CLI, or another profile
mode = "auto"
effort = "xhigh"
system-prompt-file = "~/.agents/prompts/claude-slim.md"    # a trimmed house prompt
args = "--strict-mcp-config --tools 'Agent,AskUserQuestion,Bash,Edit,EnterPlanMode,ExitPlanMode,LSP,Read,Skill,TaskCreate,TaskGet,TaskList,TaskStop,TaskUpdate,WebFetch,WebSearch,Write'"

[agents.profiles.claude-planner]                           # a specialist off the same base CLI
agent = "claude"
model = "fable"
mode = "auto"
effort = "high"
system-prompt-file = "~/.agents/prompts/claude-planner.md"  # its role, craft, and boundaries
args = "--strict-mcp-config --tools 'Bash,Read,Edit,Write,AskUserQuestion,WebFetch,WebSearch,Skill,Agent(Explore,Plan)'"  # plan and explore, nothing that ships
```

```sh
rimz agents claude            # the tuned default, as @claude
rimz agents claude-planner    # the planning specialist, as @claude-planner
```

Each field renders into the base CLI's own flag, so a profile can pin anything the CLI can pin from its command line, and nothing it can't. The one exception is `budget`, which RimZ itself enforces:

| Field | What it sets | Renders as (Claude) |
| --- | --- | --- |
| `model` | the model to run | `--model opus` |
| `effort` | reasoning effort, on the provider's own ladder | `--effort high` |
| `budget` | dollar cap for the session, or per local day with `/day` | kept and enforced by RimZ ([budgets](./budget.md)) |
| `system-prompt-file` | replace the system prompt with the role's craft and rules | `--system-prompt-file …` |
| `append-system-prompt-file` | keep the base prompt and add rules on top | `--append-system-prompt-file …` |
| `mode` | the permission posture (`auto` \| `ask` \| `plan` \| `yolo`) | see [permission modes](#set-a-permission-mode) |
| `args` | raw flags handed to the stock CLI | verbatim |

The system prompt and `args` are what make a profile targeted. There is no RimZ-specific tools setting: you narrow the toolset with the agent's own flags through `args` — `--tools` for Claude, `--sandbox` for Codex — and a narrow tool surface plus a focused prompt is what keeps a specialized agent fast and on-task.

When does a bare kind stop being enough? The moment you type the same shaping flags a second time. One planner prompt you keep reusing, a reviewer that must never commit, a cheap low-effort triage agent — each is a profile.

Override any field for one launch with the matching flag, which wins over the profile:

```sh
rimz agents claude --model opus --effort xhigh --budget 5 --system-prompt-file ./review.md
```

On budgets specifically: `--budget 5` parks the agent when its session cost reaches $5, `--budget 20/day` caps each local calendar day instead, and `rimz agents budget @coder` inspects or changes the cap while the agent runs. The same dollar-cap model scales up to loop tasks, the room, and a provider login: the [budgets guide](./budget.md) owns it.

Effort ladders are provider-specific — Claude runs up to `max`, Codex and Pi to `xhigh`. The full profile shape, inheritance between profiles, and per-field rules are in [configuration → agent profiles, commands, and teams](./configuration.md#agent-profiles-commands-and-teams); pairing several profiles by role is a [team](./teams.md).

## Set a permission mode

A suffix sets how much the agent may do before it stops to ask. Like every profile field, it renders into the provider's own flags — the suffix is shorthand for a flag you already know:

| Suffix | What it does | For example |
| --- | --- | --- |
| `-auto` | the provider's auto-accept mode for routine actions | `claude --permission-mode auto` |
| `-ask` | keep the provider's native permission prompts | no flag at all |
| `-plan` | start in plan mode | `claude --permission-mode plan` |
| `-yolo` | pass the provider's bypass flag, skipping its prompts | `claude --dangerously-skip-permissions` |

```sh
rimz agents codex-yolo      # codex --dangerously-bypass-approvals-and-sandbox
rimz agents claude-plan     # start in plan mode
rimz agents claude --yolo   # the same mode as a flag
```

Not every provider defines every mode: the built-in set is `claude-{auto,ask,plan,yolo}`, `codex-{auto,ask,plan,yolo}`, `cursor-{auto,ask,plan,yolo}`, `antigravity-{auto,ask,plan,yolo}`, `grok-{auto,ask,plan,yolo}`, `opencode-{plan,yolo}`, and `pi-{ask,plan}`, and a mode a given provider has no equivalent for keeps that provider's default behavior. Cursor's Auto posture uses its classifier-backed `--auto-review` mode; Antigravity maps Auto to `--mode accept-edits` and keeps sandboxing a separate provider flag. Grok maps Ask to `--permission-mode default`, Auto to `--permission-mode auto`, and Yolo to `--yolo`; Grok Plan adds no argv because `/plan` is interactive rather than an enforced launch posture. On the command line the same choice is a flag, `--ask` or `--yolo`, and in a profile it is the `mode` field. The exact flag each mode becomes, per provider, is in [agent support](../reference/agent-support.md).

One more suffix sits outside permissions: `-ping` opens the agent at its lowest effort to warm the provider's budget window, the building block behind scheduled window-priming ([loops](./loops.md)). The built-in pings are `claude-ping` and `codex-ping`.

## See also

- [Worktrees](./worktrees.md) — isolate a layout on its own branch so several agents run in parallel.
- [Teams](./teams.md) — pair profiles by role and launch, reopen, and resume the whole set as one unit.
- [Messaging](./messaging.md) — reach agents by handle: park, steer, schedule, and channels.
- [The sidebar](./sidebar.md) — how the room reads the cards, worktrees, and teams you launch.
- [Token Insight](./insight.md) — fleet-wide token and dollar insight: the cockpit, the provider dashboard, and `rimz stats`.
- [Budgets](./budget.md) — dollar caps on an agent, a task, a room, or a provider login, and what a park means.
- [Scripting agents](./scripting.md) — the same launcher as a supervised, exit-coded run (`-p`).
- [Configuration → profiles and teams](./configuration.md#agent-profiles-commands-and-teams) — the `agents.toml` shape behind every profile and team.
- [Agent-control reference](../reference/cli/agents.md) — the complete `rimz agents`, `worktree`, and `gc` surface.
- [Agent support](../reference/agent-support.md) — which agents RimZ drives and what each integration adds.
