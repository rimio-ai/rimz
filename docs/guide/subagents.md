# Subagents

Your agents already delegate. Claude hands a search to its `Agent` tool, Codex spawns a helper, and the parent gets back a summary instead of a window full of grep output. That is the right move: a filling context window costs more per turn and reasons less sharply, and a bounded side task is the cheapest thing to push out of it.

The provider's own subagent runs headless inside the parent's process. It has to be the parent's provider, it has no pane you can look at, and when it stalls or stops on a permission question, nothing reaches you. It has no deadline you set, and its work vanishes with the turn.

`rimz subagents` gives your agents the same move with a full agent on the other end. The parent hands a bounded prompt to a child and keeps working; the child runs in its own pane in your room, on any provider you have configured, under a deadline, with a card nested under its parent in the sidebar. When the work ends, the pane closes, the answer comes back to the parent, and the run record and transcript stay on disk.

Your agents run `rimz subagents`; you mostly do not. Your part is deciding which children exist and which agent may launch which, then watching and stopping them like any other agent in the room. This page covers that part in that order. Every flag the parent uses is in the [`subagents` reference](../reference/cli/subagents.md).

## What a launch does on your machine

A parent's `rimz subagents codex "map the auth call path"` is the same supervised background run as `rimz agents codex -p --bg --timeout 30m "map the auth call path"` from the [scripting guide](./scripting.md#what-a-run-does-on-your-machine), with the flags chosen for the parent and the child tied to it. In order:

1. It checks that the caller is an agent RimZ launched and not itself a child, that the parent's profile allows this child, that the child's reporting hooks are installed and trusted, and, for a Codex child, that the checkout has a recorded Codex directory-trust decision. It also checks the room's and the provider account's daily caps ([budgets](./budget.md)). A failure stops here, before a record or a pane exists.
2. It writes a durable run record under `~/.rimz/ws/<workspace-dir>/owned/runs/`.
3. It opens a pane running the child's own CLI, in the parent's checkout and channel, whatever directory the parent's shell has moved to. The first child splits to the right of the parent's pane and later ones stack beside it; a team member's children open in a `<view> subagents` tab after its own, eight to a tab in two equal-width columns.
4. It prints the child's petname, such as `calm-fox`, and returns. The parent keeps working.
5. When the child's work ends, or its deadline passes, its pane closes. Once every child the parent launched has settled, RimZ writes each final message to `rimz-subagents/<petname>.output` in the room's tmp directory and parks one report for the parent's next turn boundary ([how results come back](#how-results-come-back)).

Nothing else moves. The child's session file lands where its CLI always writes it, and `rimz agents show @calm-fox` and `rimz transcript @calm-fox` read the run back after the pane is gone.

Children share their parent's checkout. No worktree separates their edits from each other or from the parent, so give an editing child files no one else is touching, or have the parent use `rimz agents -p --worktree` ([choose the right tool](#choose-the-right-tool)).

## Give your agents children to launch

A parent can launch a bare agent kind (`rimz subagents codex "..."`) with no profile at all. A child profile is what makes delegation deliberate: a file under `~/.rimz/subagents/` that fixes the child's model, effort, tools, and craft prompt, so the parent picks a job rather than composing a launch. Write `~/.rimz/subagents/explorer.md`:

```markdown
---
description: Maps the code a change will touch and reports paths, invariants, and test commands. Read-only. For wide searches the caller wants out of its window.
agent: codex
model: luna
effort: medium
tools: [Bash, Read, Grep, Glob]
---

Map the code the brief names and change nothing. Report each finding with its path and symbol.
```

```sh
rimz agents validate          # check every definition, subagents included
rimz subagents profiles       # what a child can be launched from
```

The format is the one direct profiles use ([profiles](./fleet.md#profiles-shape-an-agent-for-one-job)), with two differences. A child profile's `agent:` chain stays inside `subagents/`, so it can build on another child profile or a kind but never on a profile from `agents/`. And the kind base still lives in `agents/`: a Codex child needs `~/.rimz/agents/codex.md` exactly as a direct Codex profile does. Every key is in the [definition reference](../reference/definitions.md).

Write the `description` for the parent, because it is how the parent chooses. The list RimZ gives the parent carries each profile's name and description and nothing else, since a model codename and an effort level do not tell it which child is cheaper or stronger. Say what the child does, what it will not do, and when to pick it.

Two other sources feed the same list. An `[agents.commands]` entry is launchable as a child, and a trusted project can ship child profiles in `[subagents.profiles]` of its [project config](./configuration.md#project-config), which win a name collision with yours.

## Decide who may delegate

By default every agent may launch every child. A `subagents:` list on a direct profile narrows that:

```markdown
---
description: Read code and propose a plan
agent: claude
model: fable
tools: [Bash, Read, Grep, Glob, AskUserQuestion]
subagents: [explorer]
---
```

Omit the field and delegation is unrestricted; `subagents: [explorer]` allows only `explorer`; `subagents: []` allows none. A launch outside the list is refused before any record or pane exists, including one that re-bases a listed profile onto an unlisted one with `--agent`. A profile that sets any `subagents:` list cannot also grant the native `Agent` tool, so each agent delegates through one doorway or the other, and a child profile cannot set the field at all.

Every agent RimZ launches on Claude, Codex, Qwen, or Droid learns its list at launch, in the system prompt RimZ appends: the children its profile allows, with their descriptions. When the list is `[]`, it is told to do the work itself; when nothing is configured, it is told that you enable children by adding `subagents/<name>.md` files. The paragraph points the agent at a skill named `rimz-subagents` for how to launch and collect. Other providers get no paragraph and learn their list from `rimz subagents profiles`, which applies the same filter when an agent runs it.

## Share code navigation

A native language-server tool starts a full index for every agent and child, so a parent with three children can hold four copies of the same index over one checkout. A [shared language server](./lsp.md) gives them one instead: configure it once, launch as usual, and RimZ starts it before panes open if memory permits. A child never starts a server of its own; it joins its parent's when one runs, and its launch reminder names it the same way.

## Watch and answer children

A child is an agent in your room, so the [sidebar](./sidebar.md#the-agent-card) treats it as one, nested one level under its parent. The parent's card gains a `⧉ subagents (N)` line with what the children cost; click it to list each child with its state, the task it was given, and, while it runs, its model, tokens, and elapsed time. The parent's dollar figure already includes its children's spend.

A child that stops on a permission prompt or a question flips to `? waiting` and reaches you the way any agent does: the cockpit counts it and your [notification handler](./notifications.md) fires. Answer in the child's pane. Its permission mode is the one its profile's `mode:` sets, or the automatic-approval default every supervised run starts with, so a child with a narrow tool list rarely needs to ask.

From your own shell:

```sh
rimz subagents list              # every child in the current channel, with its parent
rimz agents show @calm-fox       # one child's activity, context, and recent transcript
rimz transcript @calm-fox        # its conversation, after the pane has closed too
```

`list` from a shell outside any channel shows every child in the room. Channel and `@all` transcript views leave children out, so read a child by its handle.

A child runs one prompt and is not built to read messages mid-run. To change what a child is doing, stop it and let the parent relaunch.

## How results come back

The parent does not poll. Once every child it launched has settled, RimZ parks one message for its next turn boundary:

```text
Type: SUBAGENT_REPORT
From: @rimz
Content:
All 3 subagents settled, responses total ~5.1k tokens, 96 lines:
- @naming: completed in 4m12s, task: "map spec/profile surfaces", response: /tmp/rimz-subagents/naming.output (~4.2k tokens, 84 lines)
- @runtime: completed in 5m3s, task: "inspect runtime behavior", no response
- @slow-reviewer: timed out after 30m; provider did not stop, task: "review correctness", response: /tmp/rimz-subagents/slow-reviewer.output (<1k tokens, 12 lines)
```

The report carries status and a file path per child, never the answers themselves, so the parent decides how much of each to read into its window. A parent that needs an answer sooner joins with `rimz subagents wait <petname>`, and a child it has joined or stopped drops out of the report. The path above is the sandbox view; under host isolation the report names the host path of the room's tmp directory. Every field of the report is in the [reference](../reference/cli/subagents.md#the-fleet-report).

A parent that is itself a supervised run (`rimz agents -p`, or a scheduled task) can end its turn while its children work. Its run stays open and your script keeps blocking until the report arrives and the parent finishes.

## Deadlines, stopping, and cleanup

Every child has a deadline: 30 minutes unless the parent passes `--timeout`, and the default is yours to change ([subagent launches](./configuration.md#subagent-launches)). The room enforces it whether or not anyone is waiting, and a child that hits it reports as `timed out`.

To stop a child yourself, stop it like any agent. The parent's report lists it as `canceled`:

```sh
rimz agents stop @calm-fox
```

Stopping a parent, with `rimz agents stop` or `rimz teams stop`, stops its live children first. A parent that ends any other way takes its children with it: each child watches its parent and cancels itself when the parent's session ends, and the room sweeps up any child still running ten minutes after its parent ended.

A child launched with `--keep` is the exception. Its pane stays open after it finishes and after its parent exits, for you to read; it closes when the parent runs `rimz subagents stop`, when you stop the parent, or when you close the pane by hand.

What remains after a child ends is its run record under `~/.rimz/ws/<workspace-dir>/owned/runs/` and the session file its CLI wrote, so `rimz agents show` and the parent's `rimz subagents wait` still read the outcome. The `rimz-subagents/` answer files go when the room closes. A child has no restart or resume: to retry, the parent launches the same profile and prompt again. [Rebirth recovery](./configuration.md#resume) restores root agents but not children, and reports a child whose run was cut off as canceled to its parent.

## Children cannot delegate

A child does its assignment itself. RimZ refuses any `rimz agents`, `rimz teams`, or `rimz subagents` launch from a child, tells the child so in its prompt, and switches off the provider's own delegation where it has a verified switch: Claude's `Agent` tool is denied, Codex's `multi_agent` feature is off, and OpenCode's `task` permission is denied. Other providers get the instruction only.

That keeps delegation one level deep. A parent's children are the whole family, the sidebar shows them all under one card, and stopping the parent reaches every one. For the same reason `[agents] max-chain-length` does not count subagent launches; it limits agents launching peers, below.

## Choose the right tool

- **A provider's own subagent is right for a quick lookup you never need to see.** It is fast, runs inside the parent's process, and costs no pane.
- **`rimz subagents` is right for a bounded task you want visible or durable.** Reach for it when the task should run on another provider, needs a deadline, might stop to ask you something, or produces a result worth keeping after the turn.
- **`rimz agents -p` from an agent is right when the child needs a control `subagents` leaves out.** A separate worktree, `--stdin`, `--verify`, and `--retries` are there ([scripting](./scripting.md)). The result is an independent peer with its own top-level card, joined with `rimz agents wait`, and it counts toward the launch chain, which stops at three successive agent-started launches by default ([`max-chain-length`](./configuration.md#agent-launch-chains)). A background peer left unjoined still comes back in the parent's report.
- **A [team](./teams.md) is right when the work needs rounds.** A child answers once and is finished; team members keep their windows open and talk back and forth.
- **[`rimz message`](./messaging.md) is right when the task belongs to an agent already running.** It hands the work over without starting a fresh turn.

## See also

- [Subagents CLI](../reference/cli/subagents.md): every flag the parent uses, the `fanout` task format, the report fields, and the pane placement rules.
- [Definition reference](../reference/definitions.md): every key a child profile accepts, and the `subagents:` list rules.
- [Scripting](./scripting.md): the supervised run every child is, and the `rimz agents -p` peer launch.
- [Sidebar](./sidebar.md#the-agent-card): how children render under their parent's card.
- [Subagents internals](../internals/harness/subagents.md): parentage, the watchdog, and how the report is composed.
