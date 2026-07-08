# Agents

Rimz watches the agents you already run; it never replaces their CLIs. An agent reaches the room two ways: you type its stock command into a pane, or you launch it with `rimz agents`. Typing the command is the natural way in and the whole story for many sessions. Reach for `rimz agents` when you want an agent tuned for one job, several agents at once, or a layout dropped into its own [worktree](./worktrees.md) or [team](./teams.md).

## Run an agent directly

Type the agent's own command in any pane. It appears in the sidebar, reporting from its first line:

```sh
claude          # the stock Claude CLI
codex           # the stock Codex CLI
```

No Rimz command sits in the path. The CLI runs exactly as it always does, and Rimz's reporting hooks — installed once at the [consent gate](./quickstart.md#the-consent-gate) — read what it does: status, task, context health, live cost. From that the agent gets a live card, a handle you can message, and a place in the attention ranking. Which agents Rimz drives, and what each integration reports, is [agent support](../reference/agent-support.md).

For a single agent in the pane you are standing in, this is all you need.

## Launch through `rimz agents`

`rimz agents` launches the same stock CLIs, and earns its keystrokes when a bare command in the current pane isn't enough:

- **a customized agent** tuned for one kind of work, a [profile](#customize-an-agent-with-a-profile),
- **several agents at once**, arranged in a [layout](#compose-a-layout),
- **an isolated [worktree](./worktrees.md)** or a named **[team](./teams.md)**.

The agent still runs stock; Rimz decides where its pane lands and what identity it carries.

```sh
rimz agents claude            # the stock CLI, launched and placed by Rimz
rimz agents codex             # same for Codex
rimz agents claude,codex      # two agents, side by side
rimz agents peer              # the built-in team: claude,codex side by side
```

## Customize an agent with a profile

A bare `claude` is a general-purpose agent. Most work is not general: a planner should reason hard and edit nothing, a test-writer should stay in the test tree, a reviewer should read the diff and propose rather than commit. A **profile** is a named preset that turns the stock CLI into an agent shaped for one job — its model, reasoning effort, system prompt, and tool surface fixed up front. A shaped agent beats a general one steered by hand: fewer wrong turns, less context spent wandering, a lower bill for the same result.

Define a profile in `agents.toml`, then launch it by name:

```toml
[agents.profiles.planner]
agent = "claude"                                           # the base CLI, or another profile
model = "opus"
effort = "high"
system-prompt-file = "~/.config/rimz/prompts/planner.md"   # its role, craft, and boundaries
args = "--allowed-tools Read Grep Glob"                    # read and search only, no edits
```

```sh
rimz agents planner           # launches Claude under the planner preset, as @planner
```

The profile layers these fields over its base `agent`:

| Field | What it sets |
| --- | --- |
| `model` | the model to run |
| `effort` | reasoning effort, on the provider's own ladder |
| `system-prompt-file` | replace the agent's system prompt with the role's craft and rules |
| `append-system-prompt-file` | keep the base prompt and add rules on top |
| `mode` | the permission posture (`auto` \| `ask` \| `plan` \| `yolo`) |
| `args` | raw flags handed to the stock CLI |

The system prompt and `args` are what make a profile targeted. There is no Rimz-specific tools setting: you narrow the toolset with the agent's own flags through `args` — `--allowed-tools` for Claude, `--sandbox` for Codex — so a profile can pin anything the CLI can. A narrow tool surface plus a focused prompt is what keeps a specialized agent fast and on-task.

Override any field for one launch with the matching flag, which wins over the profile:

```sh
rimz agents claude --model opus --effort xhigh --system-prompt-file ./review.md
```

Effort ladders are provider-specific — Claude runs up to `max`, Codex and Pi to `xhigh`. The full profile shape, inheritance between profiles, and per-field rules are in [configuration → agent profiles, commands, and teams](../reference/configuration.md#agent-profiles-commands-and-teams); pairing several profiles by role is a [team](./teams.md).

## Set a permission mode

A suffix sets how much the agent may do before it stops to ask, rendered into each provider's own flags:

| Suffix | What it does |
| --- | --- |
| `-auto` | the provider's auto-accept mode for routine actions |
| `-ask` | keep the provider's native permission prompts |
| `-plan` | start in plan mode |
| `-yolo` | pass the provider's bypass flag, skipping its prompts |

Not every provider defines every mode: the built-in set is `claude-{auto,ask,plan,yolo}`, `codex-{auto,ask,plan,yolo}`, and `pi-{ask,plan}`, and a mode a given provider has no equivalent for keeps that provider's default behavior. On the command line the same choice is a flag, `--ask` or `--yolo`. The exact flag each mode becomes, per provider, is in [agent support](../reference/agent-support.md). It also sets a profile's `mode` field.

```sh
rimz agents codex-yolo      # launch straight through provider prompts
rimz agents claude-plan     # start in plan mode
rimz agents claude --yolo   # the same mode as a flag
```

One more suffix sits outside permissions: `-ping` opens the agent at its lowest effort to warm the provider's budget window, the building block behind scheduled window-priming ([loops](./loops.md)). The built-in pings are `claude-ping` and `codex-ping`.

## Compose a layout

One spec describes a whole layout of panes. **Commas split columns, plus signs tile rows, slashes stack rows** (a Zellij stack; tmux tiles them). Each cell is an agent kind, a `<kind>-<mode>` cell, a profile, a configured command, or `term` for a plain shell. An optional trailing prompt broadcasts to every agent cell in the layout.

```sh
rimz agents claude,codex                     # two agents, side by side
rimz agents claude,codex+term                # Claude | Codex tiled over a shell
rimz agents claude/codex/term                # one stack of three rows
rimz agents 'vim,codex+term'                 # your editor beside an agent stacked over a shell
rimz agents claude,codex "Draft the API shape."   # one prompt to both agents
```

Quote a spec whenever it contains a `+`, a space, or anything your shell would otherwise expand. Profiles and kinds compose the same way, so `rimz agents planner,coder+reviewer` lays out three of your presets. The full grammar and how cells compile to panes is [harness.md → The layout IR](../internals/harness/harness.md#the-layout-ir).

Add `-w` and the whole layout lands in an isolated Git worktree, the pattern for running several agents in parallel without stepping on each other: see [Worktrees](./worktrees.md).

## Handles, in brief

Every agent answers to a handle: `@claude` names a kind, `@planner` a profile, `forge.reviewer` one role of a team. Rimz gives each bare launch a stable pet name too (`@swift-otter`), and `--name writer` pins your own (`@writer`). A `#channel` suffix scopes the handle to one channel — every worktree gets one, and named channels and teams have their own — defaulting to the one you are standing in.

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
AGENT         STATUS   MODEL         CTX  TOKENS  AGE  DESC

⑂ auth-refresh · forge team
@planner      waiting  opus@high      42%     78k   2m  which rotation strategy should we use?
@coder        running  gpt-5.5@high   31%     54k   0s  wire up the refresh-token path
@reviewer     idle     opus@high       3%     12k  15m  review the diff once coder lands

query-engine
@swift-otter  success  opus@high      78%    120k   8m  store refactor
```

`@planner` sits first because it stopped to ask you something; the columns read its status, model and effort, how full its context window is, tokens used, how long since it last moved, and what it is on. Add `--all` to widen past the current channel to every lane in the room, or a scope like `rimz agents '#auth-refresh'` to read one.

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

Two everyday tasks have their own guides, with the depth this page leaves out:

- **Steer or queue an agent** — send text that parks at the turn boundary, interrupts the live turn, or arrives on a schedule: the [messaging guide](./messaging.md).
- **Script an agent** — run one supervised, exit-coded turn for a pipeline or CI job with `rimz agents … -p`: the [scripting guide](./scripting.md).

The complete `rimz agents` surface, every verb and flag, is the [agent-control reference](../reference/cli/agents.md).

## See also

- [Worktrees](./worktrees.md) — isolate a layout on its own branch so several agents run in parallel.
- [Teams](./teams.md) — pair profiles by role and launch, reopen, and resume the whole set as one unit.
- [Messaging](./messaging.md) — reach agents by handle: park, steer, schedule, and channels.
- [The sidebar](./sidebar.md) — how the room reads the cards, worktrees, and teams you launch.
- [Scripting agents](./scripting.md) — the same launcher as a supervised, exit-coded run (`-p`).
- [Configuration → profiles and teams](../reference/configuration.md#agent-profiles-commands-and-teams) — the `agents.toml` shape behind every profile and team.
- [Agent-control reference](../reference/cli/agents.md) — the complete `rimz agents`, `worktree`, and `gc` surface.
- [Agent support](../reference/agent-support.md) — which agents Rimz drives and what each integration adds.
