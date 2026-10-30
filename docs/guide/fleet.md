# Agents

RimZ works with the agent CLIs you already run: type `claude` or `codex` into a pane and the room picks it up. `rimz agents` is the other way in, for the sessions where one stock CLI is not enough: an agent shaped for one job, several launched in one line, and the commands that read and drive them once they are working.

## Run the CLI you already run

Type the agent's own command in any pane, exactly as you did before RimZ:

```sh
claude          # the stock Claude CLI
codex           # the stock Codex CLI
```

The agent's card appears in the sidebar as its first hook fires. No `rimz` command sits in the path: the CLI runs with your flags, your config, and your session files, and the [reporting hooks](./setup.md#install-agent-hooks) you approved during setup tell RimZ what it is doing. From that the agent gets a live card, a handle you can message, and a place in the attention ranking. Which agents RimZ drives, and what each one reports, is [agent support](../reference/agent-support.md).

With OpenCode 2.1.4, a plain `opencode` connected to its shared service gets one card following the conversation on screen, with its title and usage. A background tab asking for permission or an answer takes the card until answered. Clicking still jumps to the pane; messaging and other advanced controls require a `rimz agents` launch.

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

**The wrapper stays thin.** `rimz agents slim` does exactly two things on your machine. It renders the profile into the stock CLI's own flags, the `claude --permission-mode auto …` line above and nothing you could not type yourself. Then it runs that command in your Zellij or tmux, in the pane you are standing in for a single agent launched from your shell and in a fresh tab for a layout or a worktree, under a small RimZ launcher that stamps the handle and hands over to the CLI. The agent process is the official CLI, and its session files land where the CLI always puts them, so `claude --resume` and the provider's own apps keep working. A launch into your current pane also renames that tab after the profile or channel, and the name stays after the agent exits and you are back in the shell.

## Reach an agent by handle

Every agent answers to a handle, and the launcher decides it. `@claude` names a kind, `@planner` names a profile, and `forge.reviewer` names one role of a team. RimZ gives each launch a stable pet name as well (`@swift-otter`), and `--name writer` pins your own (`@writer`). A `#channel` suffix scopes a handle to one channel: every worktree has one, named channels and teams have their own, and a bare handle means the channel you are standing in.

When a generated pet name is already taken, RimZ tries other word pairs before adding a number. Existing stored names stay unchanged.

```sh
rimz agents focus @swift-otter#feat-a # jump to a specific agent's pane
rimz agents show swift-otter         # its activity, context, placement, and transcript tail
```

Every verb on this page takes one. Routing text to a handle, parking at the turn boundary, steering the live turn, broadcasting to a channel, is the [messaging guide](./messaging.md), which owns the full address grammar and the delivery rules.

## Compose a layout

Launching three agents by hand is three pane splits and three commands typed. One spec does it in one line. A comma splits columns, a plus tiles rows, a slash stacks rows with one expanded member. Focus a collapsed member to open it, on either backend:

```sh
rimz agents claude,codex                     # two agents, side by side
rimz agents claude,codex+term                # Claude | Codex tiled over a shell
rimz agents claude/codex/term                # one stack of three rows
rimz agents 'vim,codex+term'                 # your editor beside an agent tiled over a shell
rimz agents claude:planner,codex:coder -w feat-x   # ad-hoc role handles, no saved team
rimz agents claude,codex "Draft the API shape."    # the prompt lands on the first cell, Claude
```

Each cell is an agent kind, a `<kind>-<mode>` cell ([permission modes](#set-a-permission-mode)), a `-fast` cell such as `claude-fast` ([fast cells](../reference/cli/agents.md#fast-cells)), a [profile](#profiles-shape-an-agent-for-one-job), a command you configured, any executable on your `PATH`, or `term` for a plain shell; a configured command or profile wins over a same-named binary. Suffix an agent cell with `:role` and it answers to `@role` for this launch, without a saved team.

A trailing prompt goes to exactly one agent: the leader when you launched a named team, and the first agent cell otherwise. Give a repeated first cell an inline role to make that target unambiguous, and use `rimz message @all` after launch when every agent needs the same text.

Quote a spec whenever it contains a `+`, a space, or anything else your shell would expand. Profiles and kinds compose the same way, so `rimz agents planner,coder+reviewer` lays out three of your presets. Every cell form and every launch flag are in the [launch reference](../reference/cli/agents.md#launch-a-layout).

Add `-w` and the whole layout lands in an isolated Git worktree, the pattern for running several agents in parallel without stepping on each other: see [Worktrees](./worktrees.md). A set of roles you keep relaunching together earns a name of its own: see [Teams](./teams.md).

## Manage a running room

When an agent launches a peer with a prompt, it need not poll for the answer. The peer starts in the launching agent's own checkout unless `--cwd` or a worktree selects another directory. RimZ sends the launcher an `AGENT_REPORT` with the final response file when its launched work settles, and again after each turn opened by a message from that launcher. The peer keeps its pane and remains available to you; turns you type there do not report back. The launch receipt names the response file and a wait command. Providers without the needed turn hooks say in the receipt that no report will come. Use `rimz agents stop @peer` when you are finished with the peer. A team launched by an agent is different: it reports once, when its board reaches `Done` ([teams](./teams.md)). Add `--detach` to a prompted launch when the launcher should not hear back at all: no report returns for that launch and nothing holds the launcher, while `rimz agents wait` still reads the answer.

Once a few agents are working, the same `rimz agents` command reads the room and drives it: inspect, focus, stop, restart, or compact one agent. Every verb below takes a [handle](#reach-an-agent-by-handle), and they all run from any pane in the room, or from a script anywhere that resolves to the same workspace.

**See the whole room at a glance.** `rimz agents list` lists the current channel's cards; `--all` includes every channel. Cards group by the worktree each lives in and put whoever needs you on top:

The addresses in `list` and `top` work from where you are: `#channel` appears when a bare handle would reach another agent or miss. Copy the whole address, including that suffix, into `show`, `focus`, or `stop`. A branch header also shows `· #channel` when its branch name would not select that group.

```console
$ rimz agents list --all
4 agents · 2 lanes · 1 needs you · 1 running · 2 at rest · whole room

needs you
  @planner  waiting  2m  which rotation strategy should we use?

HANDLE        PROFILE   AGENT   STATUS   MODEL         CTX  TOKENS  SEEN

⑂ auth-refresh · forge team
@planner      planner   claude  waiting  Opus 5@high   42%     84k    2m
  which rotation strategy should we use?

@coder        coder     codex   running  GPT 5.5@high  31%     84k    0s
  wire up the refresh-token path

@reviewer     reviewer  claude  idle     Opus 5@high    3%      6k   15m
  review the diff once coder lands

main
@claude#main  -         claude  success  Opus 5@high   78%    156k    8m
  store refactor
```

`@planner` sits first because it stopped to ask you something. The summary and `needs you` block name everyone waiting, paused, or failed before the full tables, including parents whose current children wait or whose provider-native children fail. The block orders them oldest first and shows the ask's age, or time since the card's last activity when there is no open ask. `CTX` is how full each agent's context window is, `TOKENS` how many tokens occupy it now, and `SEEN` how long since the agent was last seen active. The indented line follows the sidebar: a pause or failure reason first, a matching working or sleeping wait, then what the agent is working on. A parent waiting on its children names them there too. `rimz agents list --all` widens past the current channel to every channel in the room, and a scope like `rimz agents list '#auth-refresh'` reads one. Listing needs the `list` verb: bare `rimz agents` refuses and prints the `list` command to run, then a map of its verbs (the same map `rimz agents --help` shows), so a launch whose spec came out empty fails instead of printing cards.

**Ask why one agent is where it is.** When a card raises a question you cannot answer from one line, `rimz agents show` prints the full report for a single agent, so you see what you asked it, what it is spending, and where its pane lives without switching to it:

```console
$ rimz agents show @coder
@coder  codex · coder · GPT 5.5@high
running (acting) · turn 3m · last seen 0s ago
wire up the refresh-token path

Context
  31% of 272k · 84k in window · 3M spent (2M cache read) · $0.8700 · 12m active
  compactions: 0
  tools:       12 · Bash 9, Read 3
  budget:      -

Identity
  session:  01JQZ8L4M2P9RT
  role:     coder
  team:     forge
  channel:  auth-refresh
  worktree: /home/you/code/query-engine-auth-refresh
  pr:       #91 open · ci passing
  pane:     tmux:%14
```

Run, Messages, and Recent transcript appear before Context when present, so recent activity comes before stable facts. Waiting cards give you the next answer command, or point to the pane when the prompt needs its own UI. Live runs show a moving duration; only a finished run has an exit code. Add `--capture` to append the pane's visible text, or `--json` for the structured record a script can read.

If the provider's screen hid a launch warning, find it under `launch_warnings` in the Identity section. A clean relaunch clears those warnings. When the provider reports an effort different from the launch's request, Identity adds an `effort` row, for example `running max, requested xhigh`. This read-time notice changes neither the saved request nor JSON output.

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

`-f` keeps following new lines across an agent restart, and `--all` reaches back into the agent's earlier sessions. An agent can read its own conversation with `rimz agents logs @me`. A newly launched agent with nothing to show reads `@coder has no messages yet`, followed by its launch age when known and `no turn opened` if it has not started a turn. For the live screen rather than messages, use `rimz agents show @coder --capture`. The [logs reference](../reference/cli/agents.md#logs) covers ordering and tail selection.

**Read what each turn cost.** `rimz agents history` joins the conversation's user turns to the provider's token and price records, so you can see which request consumed the session without leaving the agent view. A long wait's keepalive pings fold into one row so the turns that did work stay visible. `-n` keeps the newest rows; `--json` returns one record per turn, and its `-n` counts turns:

```console
$ rimz agents history @coder -n 3
START    DUR     FRESH     COST  OUTCOME  FROM          PROMPT
15:12     8m  ↘4k ↗510  $0.4210  done     user          implement refresh-token rotation
15:21  1s–3s  ↘300 ↗30  $0.0030  done     keepalive ×3  over 10m · Cache keepalive, no action needed.
15:44    12s   ↘320 ↗0  $0.0310  open     wait          run the focused integration tests
5 turns · 5k fresh + 48k cached tokens · $0.4550
```

`↘` is fresh input and `↗` output, summed over each row's turns; the footer sums these as `fresh` and counts cache reads and writes separately as `cached`. `FROM` tells you who sent the prompt, without burying its text in a delivery header. An unknown outcome reads `-`, not an interruption. Dates disappear when every shown row starts today.

**Credit the people and models behind the change.** A pull request usually opens after the first teammate has already exited, so live agent cards cannot supply a complete byline. `rimz agents attribution` reads the lane's audit records and provider transcripts instead:

```console
$ rimz agents attribution
branch feat/auth · since 2026-09-06 21:26
forge team · 3 agents · 59m active · $42.72 · 10 messages (1 from you)

  @planner · Claude · claude-fable-5-1@high
      effort:    17m active · $9.66
      subagents: 3 runs · $3.51
                 explorer  2 × · $2.40
                 designer  1 × · $1.11
      activity:  46 tool calls
      messages:  1 from you · 3 from teammates · 7 to teammates
      tokens:    469.5k input, 83.9k output, 142.1k cache write, 7.6m cache read

  @coder · Codex · gpt-6-astra@high
      effort:    33m active · $27.85
      subagents: 9 runs · $11.38
                 implementer  8 × · 8m active · $10.38
                 documenter   1 × · 2m active · $1.00
      activity:  234 tool calls
      messages:  5 from teammates · 13 to teammates
      tokens:    600.4k input, 95.6k output, 98.4k cache write, 18.9m cache read

  @reviewer · Claude · claude-opus-5@high
      effort:   9m active · $5.21
      activity: 34 tool calls
      messages: 1 from teammates · 1 to teammates
      tokens:   62 input, 37k output, 256.7k cache write, 3.4m cache read

Models
  gpt-6-astra:      $27.32 · 763.1k input, 81k output, 15.6m cache read · 67 tok/s
  claude-opus-5:    $6.93 · 192 input, 65.5k output, 355.1k cache write, 7.7m cache read · 41 tok/s
  claude-fable-5-1: $6.15 · 90 input, 45.5k output, 142.1k cache write, 4.1m cache read · 33 tok/s
  gpt-5.6-terra:    $2.32 · 527k input, 39.1k output, 4m cache read · 52 tok/s
```

The report covers the agents that worked the checkout's current branch within the worktree's current life, teammates that exited on the way included. Every dollar and token figure is all-in for the member, its subagents counted in rather than added on. The per-kind subagent block shows a share of that member's figure, with active time for pane-backed children where available. `Models` splits the same spend by model, largest first, and shows response throughput over this report's agents when timing is available. `--md` prints a collapsed block ready to paste into a pull-request body, and the [attribution reference](../reference/cli/agents.md#attribution) covers the other scopes, the branch evidence, and what the Markdown form leaves out.

**Find what is burning CPU or tokens.** When the machine gets loud, `rimz agents top` ranks the live fleet by the resources each agent's pane process tree is using. It streams by default; `--once` takes a sample and exits for a script:

```console
$ rimz agents top --once --all
5 agents · 143.0% CPU · 1.9 GB MEM · 3M spent
AGENT             STATUS     CPU     MEM    IO/S  PROCS  CTX  SPENT  SEEN
@coder            running  96.0%  892 MB  4 MB/s     12  31%     1M  0s
  └ @swift-otter  running  44.0%  600 MB  2 MB/s      8  62%     2M  1s
@planner          waiting   2.0%  410 MB       -      6  42%   200k  2m
@reviewer         idle      1.0%   50 MB       -      4   3%    12k  15m
@claude#main      success      -       -       -      -  78%   240k  8m
```

`SPENT` is each provider session's cumulative token usage, not its current window occupancy. Children nest under their parent; here `@coder` has spent 1M and its child 2M, so `show @coder` reports 3M for the whole seat. `--all` includes the same channels as the room-wide `list` above.

**Jump in, or shut one down.** A row you want to answer is one `focus` away; a pane you are done with is one `stop`:

```sh
rimz agents focus @coder        # jump to its pane
rimz agents stop @reviewer      # close the idle reviewer's pane
rimz agents stop @claude --all  # close every Claude in scope
```

`focus` confirms the destination with `focused <address>`. `stop` closes the agent's pane, ending the CLI process after its cleanup, and stops any supervised children that agent launched. If that leaves only the sidebar in the tab, the tab closes on the next refresh without waiting for the agent's cleanup. Its receipt names the stopped agent and, for a resumable solo agent or team seat, the command to resume it. Sessions stay on disk in the provider's own format. To reopen one stopped solo agent, use [`rimz agents resume @swift-otter`](../reference/cli/agents.md#resume-one-closed-session). Its exit hint names that exact handle: `resume with rimz agents resume @swift-otter`. Run it from the recorded directory to take over the current pane again; from elsewhere it opens a tab. Team members keep their team-role resume command, pinned to the stopped lane.

A peer launched by another agent can reopen this way too. It keeps its launch-chain budget, but resuming does not reconnect result reporting to its original launcher. The [single-session resume rules](../reference/cli/agents.md#resume-one-closed-session) distinguish these peers from children.

To keep the conversation but change how it runs, [`rimz agents resume @handle`](../reference/cli/agents.md#resume-one-closed-session) and [`rimz agents <spec> --continue`](../reference/cli/agents.md#continue-a-cohort) accept overrides that become saved launch settings. For example, `rimz agents resume @swift-otter --isolation host --model sonnet` changes that exact session. Cohort resume names the selected channel or worktree before opening anything; when several places match, it points to the project-root listing. A prompt can ride along with a cohort: `rimz agents claude --continue --model sonnet 'Continue the review'` reopens the conversation and queues your instruction for the agent. Later resumes keep that model. Pi and OpenCode keep their native session's saved model on plain resume and restart; other providers replay the launch model, so restate an in-session switch with `--model` to keep it. An explicit OpenCode resume model is checked and switches that session before it opens.

**Bring in a provider conversation by id.** If you started a Claude or Codex session outside RimZ, `rimz agents claude --resume=<ID>` (or `codex`) looks it up in the room's account home and reopens it in its original directory with a new handle. An id already recorded in this room keeps its old handle instead. Either takes the same launch overrides as `resume @handle`. This selects one exact session, unlike `--continue`, which chooses a matching cohort. Put the kind before `--resume`; a following word is the session id, not a prompt. Pasting the id into [`rimz agents resume <ID>`](../reference/cli/agents.md#resume-a-lane-by-place) also works when RimZ recorded it, after checking for a lane with that name first. If it instead finds an unrecorded session in a provider home, it stops without launching and prints the exact `rimz agents <kind> --resume <ID>` command so you can choose the kind or profile. Send instructions afterwards with `rimz message`. See [provider-session resume](../reference/cli/agents.md#resume-a-provider-session) for account checks and conflicts.

**Choose a conversation without its id.** `rimz agents claude --resume` opens Claude's own picker under the room's account, with your profile settings plus any launch flags such as `--model` or `--yolo`. Choose there and the card takes on the session with the settings the picker ran with, keeping its old handle if RimZ already knew it. Subagents and peers it launched before closing stay linked to it. Providers that register lazily (Codex and Kiro) bind the card at your first prompt instead. You can [back out without a relaunch or hint](../reference/cli/agents.md#resume-a-provider-session). [Picker support](../reference/agent-support.md#provider-session-resume) lists the other providers.

Agent-initiated launches, forks, resumes, and restarts leave your focus alone, so another agent's handoff does not interrupt your typing. An in-place launch becomes a split instead. Shell commands keep their usual focus behavior; `--bg` also leaves already-live agents where they are. See the [focus rule and Zellij version caveat](../reference/cli/agents.md#channel-worktree-and-placement).

**Restore a channel or worktree by place.** `rimz agents resume '#auth-refresh'` focuses the channel when every member is live (unless an agent calls it or you pass `--bg`), adds only the closed members with their roles and team prompts when part of it remains live, and rebuilds the saved team and stray panes when all are closed. Beside live members, a closed member whose team or role is gone, or whose team prompt cannot apply, returns with its solo profile when that profile still resolves; a warning names the affected handles and why. During an all-closed restore, if a team's definition is gone or no longer resolves, or its saved launch settings cannot be restored, a warning names the team and why: its members return as individual agents without the team's layout and prompt layers. It announces how many agents it is resuming before opening panes. To pick up from the board and notes instead of old conversations, add [`--fresh`](../reference/cli/agents.md#resume-a-lane-by-place): restorable closed teams get new sessions in the same place, while standalone roots and members of teams that cannot restore are skipped with relaunch commands. Use `--from-pr 42` for a locally developed pull-request worktree; bare `resume` targets the current worktree. At the project root it lists only groups with closed agents and prints a command to resume the first one, or says there is nothing to resume.

**Bounce an agent in place.** `rimz agents restart @coder` from your shell focuses the agent, replaces its pane in the same layout position, and resumes the provider session with the original profile, role, team, channel, and permission mode. The profile is rendered from the current Markdown definitions, so an edit takes effect on the bounce. When the provider has no resumable conversation, restart launches fresh and prints the replacement handle rather than hiding a rename.

**Fork an agent to try another approach.** `rimz agents fork @coder` from your shell takes over the launching pane with the full conversation under a new provider-assigned session id, in the source worktree, leaving the original session untouched and keeping its permission mode. Pass `--new-pane` to keep the launching pane, or `--new-tab` for a separate view. The fork gets a fresh pet name; `rimz agents fork @coder --name twin` pins `@twin` when you want both approaches to have memorable handles.

**Compact context before the next task.** You can type the agent's native compaction command in its pane; `rimz agents compact @coder` submits that same command from wherever you are: at once on an idle agent, and queued until the next turn boundary on a busy one. To choose what survives the summary, pass an instruction on an agent whose CLI [accepts one](../reference/cli/agents.md#compact):

```sh
rimz agents compact @coder "keep the open questions and the exact file list"
```

Without an instruction it uses your [configured compaction brief](./configuration.md#smart-compaction). There is no way to force a compaction into a live turn, and RimZ refuses a second one until the agent has taken a new user turn. The [command reference](../reference/cli/agents.md#compact) covers instruction support and the delivery output.

For hands-off work, [smart compaction](./loops.md#smart-compaction) watches the seat's context line: it asks a busy agent to wrap up, compacts at rest if needed, and continues behind queued messages. It also compacts before a message reaches an already-resting agent. Solo seats, team members, and supervised children share that policy; their provider's native window remains the backstop.

Three everyday tasks have their own guides, with the depth this page leaves out. The [messaging guide](./messaging.md) sends text that parks at the turn boundary, interrupts the live turn, or arrives on a schedule. The [scripting guide](./scripting.md) turns the same launcher into one prompt, supervised until the work ends, with one exit code for a pipeline or CI job (`rimz agents … -p`). And when you are away from the terminal, [remote → answer asks from your phone](./remote.md#answer-asks-from-your-phone) keeps the bridge behind the Claude and Codex mobile apps up with every room, so an agent's question reaches you as a push.

The complete `rimz agents` surface is the [agent-control reference](../reference/cli/agents.md).

## Profiles: shape an agent for one job

A **profile** saves the shaping you would otherwise retype: model, reasoning effort, permission mode, tools, and an optional craft prompt. Profiles are Markdown files under `~/.rimz/agents/`. On a provider that takes a system prompt, its profiles share one **kind base**, a file named after the kind that holds the system prompt they all inherit. Write the base first, as `~/.rimz/agents/claude.md`:

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

This profile has no body of its own, so it runs on the base's prompt alone; give it one and the two compose, base first. The base file is not optional on a provider whose CLI takes a system prompt, which covers Claude, Codex, OpenCode, Pi, OMP, Qwen, and MiniMax (appended to its own prompt). If neither home nor packs supplies it, `rimz agents validate` fails and names the home base file to create. A provider without a system prompt takes neither a base nor a craft. Edit a home file to change future launches; deleting it retires that override and exposes any same-named pack profile beneath it. Exclude the pack item too if you want that name to stop loading. The [Markdown definition reference](../reference/definitions.md) covers home and pack tiers, inheritance, tool translation, and defaults; a prompt the provider cannot receive fails the launch before any pane opens instead of being dropped in silence.

**See what a profile will actually run.** With a bare CLI you can read the flags you typed; after a profile inherits a base and adds prompt fragments, that final command is harder to see. `rimz agents explain planner` prints the resolved command, settings, prompt, and scoped view without launching anything:

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

A repeated `--append-system-prompt-file` keeps command-line order and replaces the profile's entire fragment list for that launch; fragments need a base `system-prompt-file`. `--effort` uses the provider's own effort flag or OpenCode's checked session variant; Hermes checks its supported level list. Unsupported effort warns and launches without it. OpenCode needs a model and refuses an effort the chosen model does not offer ([details](../internals/agents/adapter_opencode.md#model-selection)). `--fast` asks Claude, Codex, Amp, or Pi for its fast mode, which answers sooner at a higher price (Pi only on supported OpenAI models), and `--no-fast` drops a profile's `fast` for one agent; the agent keeps either choice across restart and resume. `--budget 5` parks the agent when its session cost reaches $5 and `--budget 20/day` caps each local calendar day instead; the same dollar-cap model scales up to loop tasks, the room, and a provider account in the [budgets guide](./budget.md).

To try a profile on a different provider without changing it, add `--agent`:

```sh
rimz agents planner --agent codex --model gpt-5.3-codex
```

The launched handle is still `@planner`. The profile or kind you name supplies the engine, and the original keeps the job: its written permission mode, budget, tools, allowed-tools, skills, and prompt files all carry over. Pi and OpenCode also keep the original model when the replacement names none. Other provider changes drop it with a warning, unless the replacement or launch supplies a model. The profile file stays unchanged, but the launch records the `--agent` base and restart replays it. Restart refuses only when the current profile resolves to a different provider and no saved base restores the original kind. Which field wins when both sides set one is in the [launch reference](../reference/cli/agents.md#shared-launch-params).

OpenCode tries a full `provider/model` name, then the first exact ID, then the nearest name within two edits, against its configured catalog. First-match and near-miss picks, and a `provider/model` name that only matches another provider's model ID, announce the full name on stderr and the card; write a full name to choose another. Effort selects a checked variant of that model; without a model it warns and launches without effort. Model and effort selection is verified on OpenCode 2.2.0. Models supplied by user plugins, local providers such as Ollama or LM Studio, and Copilot can be refused; ordering, resume behavior, and limits are in [OpenCode model selection](../internals/agents/adapter_opencode.md#model-selection).

Where profiles live, how they inherit from each other, and the per-field rules are in [configuration → agent profiles, commands, and teams](./configuration.md#agent-profiles-commands-and-teams); pairing several profiles by role is a [team](./teams.md).

## Set a permission mode

The permission mode is the setting you change per task rather than per profile: the reviewer you trust to read should still ask before it writes. A `<kind>-<mode>` cell sets it at launch, and like every profile field it renders into the provider's own flags, so the suffix is shorthand for a flag you already know:

| Suffix | What the agent may do | For Claude |
| --- | --- | --- |
| `-ask` | keep the provider's native permission prompts | no flag at all |
| `-auto` | auto-accept the provider's routine actions | `--permission-mode auto` |
| `-plan` | start in plan mode | `--permission-mode plan` |
| `-yolo` | skip the provider's prompts entirely | `--dangerously-skip-permissions` |

For Claude and Codex, RimZ also allows routine coordination so it need not stop for permission: messages, agent, subagent, and team commands, asks and answers, timed waits, pane inspection, loop inspection, and read-only LSP queries. Claude gets launch-only `--settings`; the same settings add your agent's scratch/shared directories and describe their use for redirected output to auto mode while retaining its built-in defaults. Codex gets an installed rules file; matching commands run unsandboxed without Guardian review. This is not a blanket permission for `rimz`: commands such as `pane send`, `wait --run`, and `loop add` get no grant from RimZ. Because agent, subagent, and team launches are allowed, an agent can start another agent with `--yolo` or a bypass flag, resume a team with `--yolo`, and it can answer another agent's prompts ([what that allows](./security.md#what-can-run-a-command)). See the [exact command list](../reference/agent-support.md#permission-modes), inspect Claude's flags with `rimz agents explain claude` or Codex's rules with `rimz hooks install --dry-run codex`, or [turn the additions off](./configuration.md#routine-rimz-commands).

In host and scoped isolation, Codex keeps its own sandbox and network choices. RimZ adds only write access for the room's directories within a workspace-write sandbox or your own permission profile. `codex-auto` uses Guardian to review requests for more access; `codex-ask` asks you instead. A profile needing full access can pass `--sandbox danger-full-access` in its args ([permission modes](../reference/agent-support.md#permission-modes)).

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
