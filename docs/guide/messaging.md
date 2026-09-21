# Messaging

To hand work to an agent that is already running, you switch to its pane and type, or you script the same keystrokes: `tmux send-keys -t %3 'rebase on main first' Enter`. That works, and it leaves two gaps. You have to know which pane holds which agent, and the text arrives the moment you send it, landing in the middle of whatever the agent was doing.

`rimz message` writes into the same pane through the same primitive and closes both gaps. You address the agent by handle, and you choose when the text lands: when its current turn ends, right now, or no earlier than a time you name.

Every send becomes a durable record before a byte reaches the pane. A busy agent, a room you close and reopen, or a failed write all leave a record you can read back, with the outcome on it, so nothing you send disappears quietly.

```sh
rimz message @claude "add coverage for the expiry edge cases"     # parks: lands when @claude's turn ends
rimz message --steer @claude "stop: the parser test comes first"  # interrupts the live turn now
rimz message --schedule 60m @codex#feat-a "run the smoke test"    # no earlier than an hour from now
rimz message @coder --wait "did the migration land? one line"     # ask, and print the reply
rimz message @all "freeze new work; I'm cutting a release"        # everyone in your channel
```

The same command serves you, your scripts, and the agents themselves: a running agent hands work to a teammate with the line you would have typed. `rimz msg` is a shorter spelling of it, and of every subcommand below.

## Address an agent

An address is `@handle[#channel]`. The handle names who; the optional `#channel` names which group of agents. Leave the channel off and RimZ uses the one your pane sits in.

A handle resolves through the shortest unique name for a running agent:

- **`@codex`** is a kind. It reaches the one Codex in your channel, and is ambiguous only when several share it.
- **`@planner`** is a profile or team role you defined in Markdown, so it reaches the right member wherever its pane sits.
- **`@swift-otter`** is a pet name, and `@codex-2` an ordinal. Either names one specific instance when a kind is not unique enough.
- **`@all`** is every agent in the channel, minus the caller when an agent sends the message.

The `@` sigil is required, so a stray word never reaches an agent: `rimz message coder "…"` fails with ``agent target `coder` must start with `@` (try `@coder`)`` rather than guessing. A raw pane id (`tmux:%1`, `zellij:terminal_3`) is the one address that needs no sigil. Handles are minted when an agent launches, and [the agents guide](./fleet.md) covers how kinds, profiles, and team roles become them.

**Reach across channels with `#channel`.** `@codex` reaches the Codex in your current channel; `@codex#feat-a` reaches the one working the `feat-a` worktree from anywhere in the workspace. The flags `--channel <name>` and `--worktree <name>` are the same restriction in flag form, and conflict with each other.

## Channels

A channel groups the agents working one line of work inside a room. The sidebar groups its cards by channel, an address targets it with `#channel`, and RimZ reopens it as its own Zellij tab or tmux window when it restores the room. Your pane always sits in one, which is why a bare `@codex` scopes to it and you only reach across when you name another.

A channel comes from one of three places:

- **A Git worktree.** Every agent working an isolated worktree shares that channel, named for the worktree ([worktrees guide](./worktrees.md)).
- **A named channel.** A durable `#design`, `#ops`, or `#release` group with no worktree behind it.
- **A team or the room directory.** A named team launched in place gets a channel of its own, and agents with no other channel fall back to the project directory's name.

`rimz channel list` shows all three, with who is in each:

```console
$ rimz channel list
CHANNEL        BACKING    AGENTS
#auth          worktree   @coder @reviewer
#design        named      @planner
#query-engine  directory  @claude
```

A named channel is one record in the room's state, and creating one opens a tab for it in the running room. There is no branch and no directory behind it, so it costs nothing to make and nothing to keep. Names take ASCII letters, numbers, `_`, and `-`.

```sh
rimz channel new design                          # register a durable #design channel
rimz agents claude --channel design "draft it"   # launch straight into the channel
rimz message @planner#design --create "plan it"  # reach into it; --create launches one if none is there
rimz channel rm design                           # remove the record; the agents in it keep running
```

Named channels and worktrees share one namespace, so a name belongs to one or the other and never both: `rimz channel new auth` is refused while a worktree named `auth` exists, and `rimz channel rm auth` sends you to [`rimz worktree remove`](./worktrees.md) instead. How channels render on screen, with their headers and groups, is [the sidebar guide](./sidebar.md); the full command surface is [cli/channel.md](../reference/cli/channel.md).

## Park, steer, or schedule

**Park for the next turn boundary.** This is what a send does with no flags at all. The text holds until the agent finishes the turn it is working on, then lands at the boundary, so it never cuts into work in flight. Use it to hand off follow-up without watching for the agent to free up. The default gate, `--on done`, waits for a finished turn: one that succeeded, went idle, or ended with the agent waiting on a timer or watch it armed. `--on any` releases the text after a failed turn too.

```sh
rimz message @coder "open a PR summary once tests pass"   # waits for the turn to end, then delivers
rimz message --on any @coder "report back either way"     # also delivers after a failed turn
```

Either way you get a receipt. It says whether the text went in now or is queued, and when a busy agent caused the queue it names the status and hands you the command that overrides it:

```console
$ rimz message @coder#auth "open a PR summary once tests pass"
queued for @coder#auth (msg_06gc05m0d327d1d0) — @coder#auth is running; send now: rimz message steer msg_06gc05m0d327d1d0

$ rimz message @planner#auth "draft the follow-up plan"
delivered to @planner#auth (msg_06gc05nhj9q2b4t7)
```

Nothing is locked in until it lands: `rimz message cancel msg_06gc05m0d327d1d0` stops a message still waiting, and `rimz message steer msg_06gc05m0d327d1d0` promotes that exact record past its gate.

**Steer the live turn.** `--steer` interrupts the agent's turn the way typing into its pane would, so you can redirect it mid-thought. If RimZ is already writing a command, message, or answer to that pane, steer waits up to 30 seconds for that write to finish, its submit key included, rather than inserting text halfway through it.

```sh
rimz message --steer @claude "stop: rebase on main first, the parser moved"
```

Steering an agent that has no live pane yet parks the text instead of dropping it, and delivery follows once the pane appears.

**Schedule a floor under delivery.** `--schedule` holds the text until a wall-clock moment: a duration in `s`, `m`, `h`, or `d`, or a 24-hour `HH:MM` time in your configured timezone, with a time already past today meaning tomorrow. The time is an earliest-delivery floor, not an appointment: once it comes due the message parks like any other and lands at the next boundary that takes it. A scheduled message stays out of the queue until it is due, so it never blocks later text to the same agent.

```sh
rimz message --schedule 90m @coder "kick off the nightly integration run"
rimz message --schedule 07:30 @planner "draft today's plan from the open issues"
```

## What a send does to your machine

Every delivery is the same short sequence against your own multiplexer. RimZ appends the record to `~/.rimz/ws/<workspace-dir>/messages/messages.jsonl`, resolves the handle to one live pane, and takes that pane's write lock so no other RimZ write interleaves. Then it pastes the text as one bracketed paste, marks the record `sent`, and presses Enter as a separate key. `rimz pane capture @coder` shows you the result in the agent's own composer. Nothing reaches the agent by any other route: no API call, no provider-side injection.

A durable record is not a delivery guarantee, and the record is where a failure shows up. A delivery that fails repeatedly before anything reaches the pane ends `abandoned`. A prompt that reaches the pane but that the agent's reporting hooks never confirm is retyped up to three times and then marked `timed_out`; a compact command in the same state is never retyped at all, since repeating `/compact` can discard the context it was meant to save.

## Why a parked message is still waiting

A parked message delivers the moment the agent can take it, which means all of these hold:

- Its `--schedule` floor has passed and every `--after` and `--when` condition is met.
- It is the oldest ready message for that agent. Messages to one agent deliver oldest first, and scheduled or condition-held ones step out of line until they are ready.
- The agent is not [compacting its context](#compact-before-the-message-lands), and its status opens the `--on` gate.
- No open prompt in its pane is holding the input. A question owns the agent's next input until it is answered, and `--force` sends past one.
- A live pane exists to receive the text, and the agent's [reporting hooks](./setup.md#install-agent-hooks) are installed, since a turn-end hook is how RimZ learns the turn ended. A send that would park for an agent without them is refused up front.

`rimz message show` names the first unmet condition and the command that clears it, so you never have to guess why a message is still sitting there:

```console
$ rimz message show msg_06gc05m0d327d1d0
msg_06gc05m0d327d1d0 — queued
  from:    you
  to:      @coder
  channel: auth
  created: 2m ago (2026-07-08T15:41:06Z)

TEXT
  open a PR summary once tests pass

TIMELINE
  EVENT   WHEN
  queued  2m ago (2026-07-08T15:41:06Z)

DELIVERY CHECK
  schedule: ok
  fifo:     ok
  agent:    ok
  gate:     closed (status running, gate done)
  ask:      ok
  pane:     ok (zellij:terminal_3)
  waiting: @coder#auth is running; gate 'done' opens at next turn end
  force now: rimz message steer msg_06gc05m0d327d1d0
```

## Hold a message until a condition is met

A message can wait on an agent other than its recipient, which is how you queue a chain of work in one go rather than coming back to start each step. Two flags do it. Both are repeatable, every condition must be met before delivery, and both combine with each other and with `--schedule`.

```sh
rimz message @planner "draft the implementation plan"
rimz message @coder --after @planner "read plan.md and start"
rimz message @planner --when '@coder running 2h' "check @coder: 2h on one turn"
```

**`--after @handle` waits for another agent to finish its queued work.** Queue the upstream work before the message that waits on it. An agent that is already idle, with nothing queued and ready for it, satisfies the condition the moment you send. That result then stays satisfied, even if the agent picks up another turn later. The message's own `--on` gate applies to the agent it waits on, so `--on done` keeps waiting after a failed turn and `--on any` releases. Each condition names exactly one existing agent, never a fan-out and never the message's own recipient.

**`--when '@handle <status> <duration>'` waits for an agent to stay in one status.** The statuses are `running`, `waiting`, `idle`, `success`, and `failed`, and the duration takes `s`, `m`, `h`, or `d`. A completed turn reads `success`, not `idle`. The condition latches the first time it is met, so a busy receiver still gets the text at its next boundary even if the watched agent has moved on since. If the watched agent ends while the condition is unmet, the message is archived with that reason. The watched agent may be the receiver itself, which is what makes a keep-warm message work: `rimz message @codex --when '@codex idle 58m' "ping"`.

An unmet condition steps the message out of the receiver's queue order, so later eligible text still lands. `rimz message show` names the agent holding the trigger, and `rimz message steer` forces the record through a missing agent or a dependency you meant to create.

## Reach several agents at once

An address that matches more than one agent is an error until you opt into the fan-out, so an ambiguous `@claude` lists the candidates instead of surprising all of them.

```sh
rimz message @all "freeze new work; I'm cutting a release"        # every agent in the channel
rimz message --all @claude "rebase on main"                      # every Claude the address matches
rimz message @codex --worktree feat-a --create "start the auth refactor"   # launch one if none exists
```

Each fan-out delivery is prefixed with the selector you typed, so what lands in every pane opens `@all, freeze new work; I'm cutting a release` and reads as a group message rather than a private one. Deliveries go out one after another, and an agent that cannot take the text right now does not hold up the rest.

When an agent sends `@all`, RimZ drops that caller and sends to its peers; with no peers it errors rather than talking to itself. An exact handle still permits a deliberate self-message, and an explicit selector fan-out such as `--all @claude` keeps every match, sender included.

`--create` launches an agent when the address matches none: a kind (`@codex`) or a profile (`@planner`) opens fresh with your text as its first prompt. `--worktree feat-a` launches it in that worktree, while an inline `#feat-a` or `--channel feat-a` registers a named channel and launches there.

## Compact before the message lands

A long turn can hit the context limit mid-message. An agent compacts by itself only once it hits its ceiling, so a prompt delivered just under that can be cut in half by a compaction firing around it. `--smart-compact` sends the agent's compact command ahead of your text once its context is full enough, including the configured summary brief where the command accepts one, so the prompt runs against a fresh window.

```sh
rimz message --smart-compact 70% @claude "now write the migration guide"   # compact first if context ≥ 70% full
```

Give a percentage of the window (`70%`) or an occupied-token count (`120000`, `180k`). Omit the flag and RimZ uses the `[harness] smart_compact` default, so you can set the behavior once and let every message inherit it ([loops → smart compaction](./loops.md#smart-compaction), [configuration → smart compaction](./configuration.md#smart-compaction)).

## Ask and wait for the reply

`--wait` turns a send into a question: RimZ delivers the text, waits through the reply turn, and prints the agent's final message on stdout. One shell command, one answer, no pane to read.

```sh
rimz message @coder --wait "did the migration land? one line"    # the reply alone on stdout
rimz message @all --wait --json "status? one line"               # one handle-keyed reply map
rimz message --all @reviewer --wait --any "first verdict?"       # return on the first reply
rimz message @codex --wait=5m "open the PR"                      # bound delivery plus turn
rimz message --steer @claude --wait "answer from this turn"      # the interrupted turn is the reply
```

A fan-out wait gathers every reply from the agents' existing contexts and streams them under `@handle:` lines in completion order, while a failed target writes its forensics to stderr and the others keep gathering. `--json` buffers one uniform map instead, whether you asked one agent or twenty. The command exits 0 only when every reply turn completed, and a deadline exits 124 with the unfinished targets marked `timed_out`; the full exit-code table is in [cli/message.md](../reference/cli/message.md#wait-for-replies).

Every target must be a running agent with installed and trusted [hooks](./setup.md#install-agent-hooks), since hooks are what report the turn's end. `--wait` conflicts with `--create`, `--schedule`, and `--no-enter`, and `--json` and `--any` each require it.

A turn that goes `waiting` or `paused` is still inside its reply, so a reply that arms a timer or a watch keeps the wait open until that fires. A bare `--wait` from your shell waits indefinitely, and from an agent it stops after an hour; pass `--wait=<duration>` when a script needs its own bound.

Agents wait on each other too, and RimZ catches the deadlock instead of hanging: a send whose target is already waiting on the caller's own reply is refused before anything is queued, and a cycle that forms inside a running fan-out fails that one target, each naming the handle and message that blocks it. Text already queued stays queued and delivers at the next boundary. An agent's own `@all --wait` gathers peer replies only, never its own.

## Send a file or piped input

Inline text is one quoted argument, and it interprets `\n` as a soft newline in the composer. For anything longer, hand RimZ the content directly.

```sh
rimz message @claude --file review-notes.md          # the file's text, newlines and all
git diff main | rimz message @reviewer --stdin "review this"
```

`--file` sends the file as written, with no escape processing and only the trailing newline dropped; an empty file is refused. `--stdin` reads stdin to EOF, trimming the surrounding whitespace. Alone it is the whole message; combined with inline text, the instruction goes first and the stdin content follows inside `<stdin>` tags. The two flags are mutually exclusive, and piped stdin without `--stdin` is ignored with a warning.

## Read and steer the queue

Bare `rimz message` opens the current channel's inbox. Every send is a record you can read back and act on after the fact:

```sh
rimz message list                       # this channel's inbox, newest first
rimz message list --all                 # every channel, grouped by #channel
rimz message list --system              # include waits, signals, subagent digests, and nudges
rimz message show msg_06gc05m0d327d1d0  # full text, timeline, and the first delivery blocker
rimz message edit msg_06gc05m0d327d1d0 --text "…"   # revise a queued message before it lands
rimz message steer msg_06gc05m0d327d1d0             # deliver now, past schedule, conditions, and gate
rimz message requeue msg_06gc05m0d327d1d0           # send a finished message again as a fresh record
rimz message cancel msg_06gc05m0d327d1d0            # cancel a queued message; the record stays
rimz message clear @coder               # cancel every open message for one agent
```

```console
$ rimz message list
you → @reviewer  queued  1m ago  msg_06gc05nhj9q2b4t7
  read plan.md and start the review · after @coder
you → @coder  queued  2m ago  msg_06gc05m0d327d1d0
  open a PR summary once tests pass
you → @planner  delivered  12m ago  msg_06gc04t1kkr7c0h9
  draft the implementation plan
... 6 system messages hidden (--system shows them)
```

The inbox shows conversation by default: your sends and attributed agent sends. The trailing line counts what the view left out, and `--system` brings in the traffic RimZ generates for itself, so you can see what woke or nudged an agent. `--limit` and `--status` narrow it further.

When something is stuck the loop is always the same: `list` to find the record, `show` to name the blocker, then `steer`, `edit`, or `cancel` to clear it.

Nine statuses cover a record's whole life. `queued` and `claimed` are still live; `sent` means the bytes reached the pane and `delivered` that the prompt's turn started; `canceled` means you stopped it and `archived` that the receiver, its channel, or a watched agent ended first; `timed_out`, `errored`, and `abandoned` are the three failures. [cli/message.md](../reference/cli/message.md#message-statuses) defines each. A finished record keeps its text in the recent history, which is what lets `show` print it and `requeue` send it again; `requeue` refuses a record whose text has aged out of that history.

`message steer` overrides every condition above: it delivers past the schedule, the conditions, the queue order, and the gate. An open prompt still stops it unless you add `--force`.

## Agents message each other

`rimz message` is the same command whether you type it or an agent runs it, so a teammate hands work over exactly as you do. The receiving agent sees a three-line header above the text, naming where the prompt came from:

```text
Type: AGENT_MESSAGE
From: @planner
Content:
read plan.md and start
```

Your sends arrive as `USER_MESSAGE` from `@user`. RimZ's own deliveries come from `@rimz`, with the `Type` naming what produced them:

- `SUBAGENT_REPORT`, the digest sent once an agent's launched children have all settled.
- `WAIT`, a [`rimz wait`](./loops.md) delivery coming due.
- `SIGNAL`, a signal-triggered loop firing.
- `STAGE`, a [team](./teams.md) stage opening.

`--no-from` delivers the text with no header at all, for a script that wants the raw bytes.

An agent's message lands as an ordinary conversation line, so the exchange belongs to the receiver's history like any other. `rimz transcript` reads a channel back, with each exchange grouped under the message that opened it. Its human view leaves out RimZ's own automation traffic; `rimz transcript --json` keeps everything. Name a launched child to read its separate conversation: `rimz transcript @swift-otter` ([transcript CLI](../reference/cli/transcript.md)).

## Asks and answers

An agent holding a permission prompt, a plan approval, or a question has handed that prompt its input, and your queued messages wait behind it until it is answered. Answering is part of keeping the fleet moving, so RimZ lets you do it from any shell.

```sh
rimz asks                          # open prompts in this channel; --all for every channel
rimz asks --json                   # the same prompts as structured data
rimz asks show @planner            # one prompt with its context and choices
rimz answer @planner allow         # submit a choice by label
rimz answer ask_06gc05m0d327d1d0 2 # or by position, against one exact prompt
```

With nothing blocked, `rimz asks` says so in one line and names `--all` when the list was scoped to your channel. `rimz answer` drives the agent's own prompt UI with the keys you would press in the pane, and confirms the agent moved on before it returns. One command submits the whole ask: a positional selector per question, commas inside one for a multi-select, and `--text` for free text. An ask with several questions can take mixed choices and text through `--json`. The ask id is a compare-and-swap token, so a prompt that was already answered or superseded receives no stale keystrokes.

Claude, Codex, and Pi accept answers this way, each within limits set by what its prompt can confirm. A Claude permission ask offers `allow`; denial and persistent grants stay in the pane. A plan approval offers the caution-marked `approve`, which turns on auto-accept edits; keep-planning and refinement text stay in the pane. [cli/asks.md](../reference/cli/asks.md#what-each-agent-accepts) has the per-agent table. You can always answer in the pane instead, and if you Escape a question and type free text there, RimZ records that text as the open question's answer rather than a new prompt.

## See also

- [Agents](./fleet.md): how handles and profiles come to be.
- [Teams](./teams.md): how team roles become handles in a shared channel.
- [Worktrees](./worktrees.md): the isolated tree behind every worktree channel.
- [The sidebar](./sidebar.md): reading the channels and the messages they carry on screen.
- [Scripting agents](./scripting.md): turning the same handles into one-shot runs with an exit code for a pipeline.
- [Loops and schedules](./loops.md): scheduled and handler-driven messages that steer the fleet unattended.
- [cli/message.md](../reference/cli/message.md), [cli/asks.md](../reference/cli/asks.md), [cli/channel.md](../reference/cli/channel.md): the exact flags.
- [Message internals](../internals/harness/messaging.md): the delivery engine underneath.
