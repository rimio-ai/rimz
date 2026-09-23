# Agents

RimZ works with the agent CLIs you already run: type `claude` or `codex` into a pane and the room picks it up. `rimz agents` is the other way in, for the sessions where one stock CLI is not enough: an agent shaped for one job, several launched in one line, and the commands that read and drive them once they are working.

## Run the CLI you already run

Type the agent's own command in any pane, exactly as you did before RimZ:

```sh
claude          # the stock Claude CLI
codex           # the stock Codex CLI
```

The agent's card appears in the sidebar as its first hook fires. No `rimz` command sits in the path: the CLI runs with your flags, your config, and your session files, and the [reporting hooks](./setup.md#install-agent-hooks) you approved during setup tell RimZ what it is doing. From that the agent gets a live card, a handle you can message, and a place in the attention ranking. Which agents RimZ drives, and what each one reports, is [agent support](../reference/agent-support.md).

## Why `rimz agents`

A bare `claude` is a general-purpose agent, and most work is not general. A planner should reason hard and edit nothing, a reviewer should read the diff and propose rather than commit, a test-writer should stay in the test tree. Even the everyday default is worth shaping, and the stock CLI takes all of it through its own flags:

```sh
claude --permission-mode auto --effort high \
       --system-prompt-file ~/.rimz/prompts/slim.md \
       --tools 'Bash,Edit,Glob,Grep,Read,Skill,WebFetch,WebSearch,Write'
```

A shaped agent beats a general one steered by hand: fewer wrong turns, less context spent wandering, a lower bill for the same result. What does not scale is the typing. That stack has to be re-entered in every pane, kept in sync across sessions, and remembered per provider, because Codex spells the same ideas differently.

`rimz agents` bottles the stack. Save it once as a [profile](#profiles-shape-an-agent-for-one-job) named `slim`, and the launcher replays it:

```sh
rimz agents slim              # the flag stack above, as one word, plus a @slim handle
rimz agents launch slim       # explicit launch verb, same agent payload
rimz agents slim,codex        # two agents, side by side in one line
```

**The wrapper stays thin.** `rimz agents slim` does exactly two things on your machine. It renders the profile into the stock CLI's own flags, the `claude --permission-mode auto …` line above and nothing you could not type yourself. Then it runs that command in your Zellij or tmux, in the pane you are standing in for a single agent and in a fresh tab for a layout or a worktree, under a small RimZ launcher that stamps the handle and hands over to the CLI. The agent process is the official CLI, and its session files land where the CLI always puts them, so `claude --resume` and the provider's own apps keep working. A launch into your current pane also renames that tab after the profile or channel, and the name stays after the agent exits and you are back in the shell.

## Reach an agent by handle

Every agent answers to a handle, and the launcher decides it. `@claude` names a kind, `@planner` names a profile, and `forge.reviewer` names one role of a team. RimZ gives each launch a stable pet name as well (`@swift-otter`), and `--name writer` pins your own (`@writer`). A `#channel` suffix scopes a handle to one channel: every worktree has one, named channels and teams have their own, and a bare handle means the channel you are standing in.

```sh
rimz agents focus @claude-2#feat-a   # jump to a specific agent's pane
rimz agents show swift-otter         # its activity, context, placement, and transcript tail
```

Every verb on this page takes one. Routing text to a handle, parking at the turn boundary, steering the live turn, broadcasting to a channel, is the [messaging guide](./messaging.md), which owns the full address grammar and the delivery rules.

## Compose a layout

Launching three agents by hand is three pane splits and three commands typed. One spec does it in one line. A comma splits columns, a plus tiles rows, a slash stacks rows (a Zellij stack; tmux tiles them):

```sh
rimz agents claude,codex                     # two agents, side by side
rimz agents claude,codex+term                # Claude | Codex tiled over a shell
rimz agents claude/codex/term                # one stack of three rows
rimz agents 'vim,codex+term'                 # your editor beside an agent tiled over a shell
rimz agents claude:planner,codex:coder -w feat-x   # ad-hoc role handles, no saved team
rimz agents claude,codex "Draft the API shape."    # the prompt lands on the first cell, Claude
```

Each cell is an agent kind, a `<kind>-<mode>` cell ([permission modes](#set-a-permission-mode)), a [profile](#profiles-shape-an-agent-for-one-job), a command you configured, any executable on your `PATH`, or `term` for a plain shell; a configured command or profile wins over a same-named binary. Suffix an agent cell with `:role` and it answers to `@role` for this launch, without a saved team.

A trailing prompt goes to exactly one agent: the leader when you launched a named team, and the first agent cell otherwise. Give a repeated first cell an inline role to make that target unambiguous, and use `rimz message @all` after launch when every agent needs the same text.

Quote a spec whenever it contains a `+`, a space, or anything else your shell would expand. Profiles and kinds compose the same way, so `rimz agents planner,coder+reviewer` lays out three of your presets. Every cell form and every launch flag are in the [launch reference](../reference/cli/agents.md#launch-a-layout).

Add `-w` and the whole layout lands in an isolated Git worktree, the pattern for running several agents in parallel without stepping on each other: see [Worktrees](./worktrees.md). A set of roles you keep relaunching together earns a name of its own: see [Teams](./teams.md).

## Manage a running room

Once a few agents are working, the same `rimz agents` command reads the room and drives it: inspect, focus, stop, restart, or compact one agent. Every verb below takes a [handle](#reach-an-agent-by-handle), and they all run from any pane in the room, or from a script anywhere that resolves to the same workspace.

**See the whole room at a glance.** Bare `rimz agents` lists the current channel's cards, grouped by the worktree each lives in and ordered so whoever needs you sits on top:

```console
$ rimz agents
AGENT         STATUS   MODEL         CTX  TOKENS  AGE

⑂ auth-refresh · forge team
@planner      waiting  opus@high     42%     78k   2m
  which rotation strategy should we use?

@coder        running  gpt-5.5@high  31%     54k   0s
  wire up the refresh-token path

@reviewer     idle     opus@high      3%     12k  15m
  review the diff once coder lands

query-engine
@swift-otter  success  opus@high     78%    120k   8m
  store refactor
```

`@planner` sits first because it stopped to ask you something. `CTX` is how full each agent's context window is and `AGE` how long since it last moved; the indented line under a row is what that agent is working on. `rimz agents --all` widens past the current channel to every lane in the room, and a scope like `rimz agents '#auth-refresh'` reads one.

**Ask why one agent is where it is.** When a card raises a question you cannot answer from one line, `rimz agents show` prints the full report for a single agent, so you see what you asked it, what it is spending, and where its pane lives without switching to it:

```console
$ rimz agents show @coder
Agent
  handle:  @coder
  kind:    codex
  role:    coder
  team:    forge
  session: 01JQZ8L4M2P9RT

Activity
  description:   wire up the refresh-token path
  status:        running
  phase:         acting
  turn_started:  2026-07-08T15:41:02Z
  turn_elapsed:  3m ago
  last_activity: 0s ago

Context
  model:              gpt-5.5@high
  fill:               31%
  window:             272000
  total_tokens:       54210
  fresh_input_tokens: 4180
  cache_read_tokens:  47600
  cache_write_tokens: 1920
  output_tokens:      510
  compactions:        0
  tools:              12 · Bash 9, Read 3
  active:             12m
  cost:               $0.8700
  budget:             -

Placement
  channel:  auth-refresh
  worktree: /home/you/code/query-engine-auth-refresh
  pr:       #91 open · ci passing
  pane:     tmux:%14

…
```

The report ends with the agent's recent transcript, cut from the block above. Add `--capture` to append the pane's visible text, or `--json` for the structured record a script can read.

**Read along without leaving your pane.** `rimz agents logs` tails an agent's transcript. Your own prompts appear as `user → @coder`, the agent's replies under its handle, so you watch a long run from the pane you are already in:

```console
$ rimz agents logs @coder -n 3
#auth-refresh

@coder  15:36
Rotating the refresh token first, then wiring the retry path.

 user  → @coder  15:38
after your turn, add coverage for the expiry edge cases

@coder  15:41
Added tests/auth/refresh_expiry.rs covering sliding and hard expiry.
```

`-f` follows new lines as the turn writes them, and `--all` reaches back into the agent's earlier sessions.

**Read what each turn cost.** `rimz agents history` joins the conversation's user turns to the provider's token and price records, so you can see which request consumed the session without leaving the agent view. `-n` keeps the newest rows and `--json` returns the same records to a script:

```console
$ rimz agents history @coder -n 3
START             DUR    TOKENS     COST  OUTCOME  PROMPT
2026-07-08 15:12   8m  ↘4k ↗510  $0.4210  done     implement refresh-token rotation
2026-07-08 15:38   3m  ↘1k ↗284  $0.2870  done     add coverage for the expiry edge cases
2026-07-08 15:44  12s   ↘320 ↗0  $0.0310  open     run the focused integration tests
3 turns · 54k tokens · $0.7390
```

`↘` is the fresh input the turn sent and `↗` the tokens it produced; the `TOKENS` total on the last line counts cache reads and writes too, which is why it dwarfs the rows.

**Credit the people and models behind the change.** A pull request usually opens after the first teammate has already exited, so live agent cards cannot supply a complete byline. `rimz agents attribution` reads the lane's audit records and provider transcripts instead:

```console
$ rimz agents attribution
branch feat/auth · since 2026-09-06 21:26
forge team · 3 agents · 59m active · $42.72 · 10 messages (1 from you)

  @planner · Claude · claude-fable-5-1@high
      effort:    17m active · $9.66
      subagents: 2 × explorer, 1 × designer · $3.51
      activity:  46 tool calls
      messages:  1 from you · 3 from teammates · 7 to teammates
      tokens:    469.5k input, 83.9k output, 142.1k cache write, 7.6m cache read

  @coder · Codex · gpt-6-astra@high
      effort:    33m active · $27.85
      subagents: 8 × implementer, 1 × documenter · $11.38
      activity:  234 tool calls
      messages:  5 from teammates · 13 to teammates
      tokens:    600.4k input, 95.6k output, 98.4k cache write, 18.9m cache read

  @reviewer · Claude · claude-opus-5@high
      effort:   9m active · $5.21
      activity: 34 tool calls
      messages: 1 from teammates · 1 to teammates
      tokens:   62 input, 37k output, 256.7k cache write, 3.4m cache read

Models
  gpt-6-astra:      $27.32 · 763.1k input, 81k output, 15.6m cache read
  claude-opus-5:    $6.93 · 192 input, 65.5k output, 355.1k cache write, 7.7m cache read
  claude-fable-5-1: $6.15 · 90 input, 45.5k output, 142.1k cache write, 4.1m cache read
  gpt-5.6-terra:    $2.32 · 527k input, 39.1k output, 4m cache read
```

The report covers the agents that worked the checkout's current branch within the worktree's current life, teammates that exited on the way included. Every dollar and token figure is all-in for the member, its subagents counted in rather than added on, and `Models` splits that same spend by model, largest first. `--md` prints a collapsed block ready to paste into a pull-request body, and the [attribution reference](../reference/cli/agents.md#attribution) covers the other scopes, the branch evidence, and what the Markdown form leaves out.

**Find what is burning CPU or tokens.** When the machine gets loud, `rimz agents top` ranks the live fleet by the resources each agent's pane process tree is using. It streams by default; `--once` takes a sample and exits for a script:

```console
$ rimz agents top --once
4 agents · 1 running · 99.0% CPU · 1.4 GB MEM · 264k tokens
AGENT         STATUS     CPU     MEM    IO/S  PROCS  CTX  TOKENS  AGE
@coder        running  96.0%  892 MB  4 MB/s     12  31%     54k  0s
@planner      waiting   2.0%  410 MB       -      6  42%     78k  2m
@reviewer     idle      1.0%  180 MB       -      4   3%     12k  15m
@swift-otter  success      -       -       -      -  78%    120k  8m
```

**Jump in, or shut one down.** A row you want to answer is one `focus` away; a pane you are done with is one `stop`:

```sh
rimz agents focus @coder        # jump to its pane
rimz agents stop @reviewer      # close the idle reviewer's pane
rimz agents stop @claude --all  # close every Claude in scope
```

`stop` closes the agent's pane, which ends the CLI process with it, and stops any supervised children that agent launched. Sessions stay on disk in the provider's own format, so a stopped agent is one `--resume` away. To keep the conversation but change how the next run works, [`rimz agents <spec> --resume`](../reference/cli/agents.md#resume-a-cohort) accepts permission, model, and effort overrides for that run.

**Restore a lane by place.** `rimz agents resume '#auth-refresh'` focuses the lane when every member is live, adds only the closed members when part of it remains live, and rebuilds the saved team and stray panes when all are closed. To pick up from the board and notes instead of old conversations, add [`--fresh`](../reference/cli/agents.md#resume-a-lane-by-place): closed teams get new sessions in the same lane, while standalone roots are skipped with relaunch commands. Use `--from-pr 42` for a locally developed pull-request lane; bare `resume` targets the current worktree, or lists the resumable lanes when you run it at the project root.

**Bounce an agent in place.** `rimz agents restart @coder` focuses the agent, replaces its pane in the same layout position, and resumes the provider session with the original profile, role, team, channel, and permission mode. The profile is rendered from the current Markdown definitions, so an edit takes effect on the bounce. When the provider has no resumable conversation, restart launches fresh and prints the replacement handle rather than hiding a rename.

**Fork an agent to try another approach.** `rimz agents fork @coder` takes over the launching pane with the full conversation under a new provider-assigned session id, in the source worktree, leaving the original session untouched and keeping its permission mode. Pass `--new-pane` to keep the launching pane, or `--new-tab` for a separate view. The fork gets a fresh pet name; `rimz agents fork @coder --name twin` pins `@twin` when you want both approaches to have memorable handles.

**Compact context before the next task.** You can type the agent's native compaction command in its pane; `rimz agents compact @coder` submits that same command from wherever you are: at once on an idle agent, and queued until the next turn boundary on a busy one. To choose what survives the summary, pass an instruction on an agent whose CLI accepts one (Claude, Grok, and Droid today):

```sh
rimz agents compact @coder "keep the open questions and the exact file list"
```

Without an instruction it uses your [configured compaction brief](./configuration.md#smart-compaction). There is no way to force a compaction into a live turn, and RimZ refuses a second one until the agent has taken a new user turn. The [command reference](../reference/cli/agents.md#compact) covers instruction support and the delivery output.

Three everyday tasks have their own guides, with the depth this page leaves out. The [messaging guide](./messaging.md) sends text that parks at the turn boundary, interrupts the live turn, or arrives on a schedule. The [scripting guide](./scripting.md) turns the same launcher into one prompt, supervised until the work ends, with one exit code for a pipeline or CI job (`rimz agents … -p`). And when you are away from the terminal, [remote → answer asks from your phone](./remote.md#answer-asks-from-your-phone) keeps the bridge behind the Claude and Codex mobile apps up with every room, so an agent's question reaches you as a push.

The complete `rimz agents` surface is the [agent-control reference](../reference/cli/agents.md).

## Profiles: shape an agent for one job

A **profile** saves the shaping you would otherwise retype: model, reasoning effort, permission mode, tools, and an optional craft prompt. Profiles are Markdown files under `~/.rimz/agents/`, and each provider's profiles share one **kind base**, a file named after the kind that holds the system prompt they all inherit. Write the base first, as `~/.rimz/agents/claude.md`:

```markdown
---
description: Claude, shaped for this machine
---

Prefer small, reviewable changes. Name what you are unsure about.
```

A base takes no settings, only a description and a prompt body. The settings go in the profile beside it, `~/.rimz/agents/planner.md`:

```markdown
---
description: Read code and propose a plan
agent: claude
model: fable
tools: [Bash, Read, Grep, Glob, AskUserQuestion]
subagents: []
---
```

```sh
rimz agents validate          # check every definition on this machine
rimz agents planner           # launch it
```

This profile has no body of its own, so it runs on the base's prompt alone; give it one and the two compose, base first. The base file is not optional on a provider whose CLI takes a system prompt, which covers Claude, Codex, Pi, and Qwen. Without it, `rimz agents validate` fails, naming the file to write: `runs on claude, whose kind base is missing — create /home/me/.rimz/agents/claude.md with a nonempty body, the system prompt every claude definition starts from`. Edit a file to change future launches, and delete it to retire the profile. The [Markdown definition reference](../reference/definitions.md) covers the frontmatter, inheritance, tool translation, and defaults; a field the provider cannot express fails the launch before any pane opens instead of being dropped in silence.

**See what a profile will actually run.** With a bare CLI you can read the flags you typed; after a profile inherits a base and adds prompt fragments, that final command is harder to see. `rimz agents explain planner` prints the resolved command, settings, prompt, and sandbox view without launching anything:

```sh
rimz agents explain planner                  # the command this profile resolves to
rimz agents explain planner --prompt > prompt.md   # the composed prompt and RimZ reminder
rimz agents explain @coder                   # a live agent's restart posture under current config
```

The [explain reference](../reference/cli/agents.md#explain-a-launch) covers overrides, redaction, and reminders a provider cannot receive.

Override a profile field for one launch with its matching flag, which wins over the file:

```sh
rimz agents planner --model opus --effort high --budget 5
rimz agents planner --append-system-prompt-file ./local.md --append-system-prompt-file ./task.md
```

A repeated `--append-system-prompt-file` keeps command-line order and replaces the profile's entire fragment list for that launch; fragments need a base `system-prompt-file`. `--effort` is passed to the provider's own effort flag without validation, so the levels are whatever that CLI accepts. `--budget 5` parks the agent when its session cost reaches $5 and `--budget 20/day` caps each local calendar day instead; the same dollar-cap model scales up to loop tasks, the room, and a provider account in the [budgets guide](./budget.md).

To try a profile on a different provider without changing it, add `--agent`:

```sh
rimz agents planner --agent codex --model gpt-5.3-codex
```

The launched handle is still `@planner`. The profile or kind you name supplies the engine, and the original keeps the job: its permission mode, budget, skills, and prompt files all carry over. The swap applies to that launch alone and is never written back, so a later restart refuses the provider mismatch and points at a fresh `--agent` launch. Which field wins when both sides set one is in the [launch reference](../reference/cli/agents.md#shared-launch-params).

Where profiles live, how they inherit from each other, and the per-field rules are in [configuration → agent profiles, commands, and teams](./configuration.md#agent-profiles-commands-and-teams); pairing several profiles by role is a [team](./teams.md).

## Set a permission mode

The permission mode is the setting you change per task rather than per profile: the reviewer you trust to read should still ask before it writes. A `<kind>-<mode>` cell sets it at launch, and like every profile field it renders into the provider's own flags, so the suffix is shorthand for a flag you already know:

| Suffix | What the agent may do | For Claude |
| --- | --- | --- |
| `-ask` | keep the provider's native permission prompts | no flag at all |
| `-auto` | auto-accept the provider's routine actions | `--permission-mode auto` |
| `-plan` | start in plan mode | `--permission-mode plan` |
| `-yolo` | skip the provider's prompts entirely | `--dangerously-skip-permissions` |

```sh
rimz agents claude-plan     # start in plan mode
rimz agents codex-yolo      # codex --dangerously-bypass-approvals-and-sandbox
rimz agents claude --yolo   # the same mode as a flag
```

The same choice is `--ask` or `--yolo` on the command line, and the `mode` field in a profile, where it becomes that profile's default. Every kind takes `-ask` and `-plan`; `-auto` and `-yolo` exist wherever the provider ships a flag for them, and a mode a provider has no equivalent for launches with that provider's own default. The exact flag each mode becomes, per agent, is the [permission-mode table](../reference/agent-support.md#permission-modes).

## See also

- [Worktrees](./worktrees.md): isolate a layout on its own branch so several agents run in parallel.
- [Teams](./teams.md): pair profiles by role and launch, reopen, and resume the whole set as one unit.
- [Messaging](./messaging.md): reach agents by handle, and park, steer, schedule, and use channels.
- [The sidebar](./sidebar.md): how the room reads the cards, worktrees, and teams you launch.
- [Token Insight](./insight.md): fleet-wide token and dollar insight, the cockpit, the provider dashboard, and `rimz stats`.
- [Budgets](./budget.md): dollar caps on an agent, a task, a room, or a provider account, and what a park means.
- [Scripting agents](./scripting.md): the same launcher as a supervised, exit-coded run (`-p`).
- [Configuration → profiles and teams](./configuration.md#agent-profiles-commands-and-teams): where reusable profiles and teams live.
- [Agent-control reference](../reference/cli/agents.md): the complete `rimz agents` surface.
- [Agent support](../reference/agent-support.md): which agents RimZ drives and what each integration adds.
