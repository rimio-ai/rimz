# Messaging

`rimz message` (`rimz msg` for short) types text into a running agent's own pane, the same way you would. It reads like Slack — every agent answers to a handle — and it guarantees the text lands: parked by default (held until the agent's current turn finishes), right now with `--steer`, or at a wall-clock time with `--schedule`.

The same command serves you, your scripts, and the agents themselves — agents talk to each other through it too. One prompt reaches one teammate; `@all` reaches everyone in your channel.

```sh
rimz message @claude "add coverage for the expiry edge cases"      # parks: lands when @claude's turn ends
rimz message --steer @claude "stop: the parser test comes first"   # interrupts the live turn now
rimz message --schedule 60m @codex#feat-b "run the smoke test"     # lands in an hour
rimz message @all "summarize what changed at the next boundary"    # everyone in the current channel
```

## Address an agent

An address is `@handle[#channel]`. The handle names who; the optional `#channel` names which group of agents — every worktree gets a channel automatically, and named channels group agents across them ([Channels below](#channels)). Leave it off and RimZ uses the channel your pane is in.

A handle resolves through the shortest unique name for a running agent:

- **`@codex`** — a kind. Reaches the one Codex in your channel; ambiguous only when several share it.
- **`@planner`** — a profile or team role you defined in `agents.toml`, so `@planner` reaches the right member wherever its pane sits.
- **`@swift-otter`** — a pet name or `@codex-2` ordinal, naming one specific instance when a kind isn't unique enough.
- **`@all`** — every agent in the channel.

Handles are assigned when an agent launches — how kinds, profiles, and team roles become handles is [the agents guide](./agents.md). The `@` sigil is required, so a stray word never broadcasts; a bare selector fails with a `did you mean @…?` hint. A raw pane id is the one exception that needs no sigil.

**Reach across channels with `#channel`.** `@codex` reaches the Codex in your current channel; `@codex#feat-a` reaches the one working the `feat-a` worktree from anywhere in the workspace. `#channel` is a suffix on the handle, and the flags `--channel <name>` and `--worktree <name>` are the same restriction in flag form.

## The three send modes

Every message becomes a durable record the instant you send it, so a busy agent, a room that closes and reopens, or a dropped multiplexer write never loses your text. The three modes differ only in *when* that record delivers:

| mode | flag | when it lands |
|------|------|---------------|
| park | default | at the turn boundary — held until the agent's current turn finishes |
| steer | `--steer` | immediately, interrupting the live turn |
| schedule | `--schedule <when>` | at a wall-clock moment: a duration (`90m`) or a time (`07:30`) |

**Park for the next turn (the default).** The text holds until the agent finishes its current turn, then lands at the boundary — it never cuts into work in flight. This is how you hand off follow-up without watching for the agent to free up.

```sh
rimz message @codex "open a PR summary once tests pass"   # waits for Codex's turn to end, then delivers
rimz message --on any @codex "report back either way"     # also delivers after a failed turn, not only a clean one
```

By default a parked message waits for a successful or idle turn (`--on done`); `--on any` releases it after a failure too.

**Steer the live turn now.** `--steer` interrupts the pane immediately, the way typing into it would, so you can redirect an agent mid-thought.

```sh
rimz message --steer @claude "stop — rebase on main first, the parser moved"
```

When `--steer` resolves to an agent that has no live pane yet, it parks the text rather than dropping it, and the retry path delivers when the pane appears.

**Schedule for later.** `--schedule` parks the text until a wall-clock moment: a duration (`s`, `m`, `h`, `d`) or an `HH:MM` time in your configured timezone. A scheduled message never blocks a later message to the same agent — it stays out of the queue until it comes due.

```sh
rimz message --schedule 90m @codex "kick off the nightly integration run"
rimz message --schedule 07:30 @planner "draft today's plan from the open issues"
```

### When a parked message lands

A parked message delivers the moment the agent can take it. All of these hold:

- The turn boundary is open — a successful or idle turn for `--on done`, plus failures for `--on any`.
- The agent isn't holding a question for you. An open prompt reserves the next input for your answer; `--force` sends past it.
- It's the agent's turn for this text — messages to one agent deliver oldest first.
- A live pane exists to receive it, and the agent's reporting hooks are installed, since hooks are how RimZ learns the turn ended.

`rimz message show msg_…` names the first unmet condition when a message is still waiting, so you never have to guess why.

### Gate on another agent's turn

`--after @handle` holds a message until that agent finishes its queued work. Repeat the flag to wait for several agents; every condition must finish before delivery.

```sh
rimz message @planner "draft the implementation plan"
rimz message @coder --after @planner "planner's done — read plan.md and start"
```

Queue the upstream work before its trigger. An agent that is already idle with no schedule-ready queued work satisfies the condition immediately, and that durable result stays satisfied if the agent starts another turn later. A condition waits while the referenced agent is running, waiting, or has undelivered ready work; future scheduled work does not hold it.

The message's `--on` gate also applies to each referenced agent: `--on done` waits after a failure, while `--on any` releases after success, idle, or failure. An unmet `--after` message steps out of the receiver's FIFO like a future scheduled message, so later eligible text still lands. `message show` names the agent holding the trigger, and `message steer` forces the record through a missing agent or an intentional dependency cycle.

## Reach several at once

A handle that matches more than one agent is an error until you opt into the fan-out — so an ambiguous `@claude` lists the candidates instead of surprising all of them.

```sh
rimz message @all "freeze new work; I'm cutting a release"          # everyone in the channel
rimz message --all @claude "rebase on main"                         # every Claude the address matches
rimz message @codex#feat-a --create "start on the auth refactor"    # launch one if none exists, this text as its first prompt
```

`@all` or `--all` fans out to every match, pacing deliveries so each agent reads a clean group message and skipping any that's momentarily blocked. `--create` launches the agent when the address matches none: a kind or profile opens fresh in the target channel with your text as its first prompt.

## Land against a fresh window

A long turn can hit the context limit mid-message. Smart compaction sends `/compact` ahead of your text once the agent's context is full enough, so the prompt runs against a fresh window instead of racing the agent's own compaction.

```sh
rimz message --smart-compact 70% @claude "now write the migration guide"   # compact first if context ≥ 70% full
```

Give a percentage of the window (`70%`) or an occupied-token count (`120000` or `180k`). Omit the flag and RimZ uses the `[harness] smart_compact` default from your config — set it once and every message inherits the behavior ([configuration → smart compaction](./configuration.md#smart-compaction), [setup guide](./setup.md)).

## Ask and wait for the reply

Add `--wait` to ask one agent or scatter the same question across a fan-out and gather the replies from their existing contexts. RimZ parks or sends each durable prompt normally, waits through every reply turn, and exits after the join settles:

```sh
rimz message @coder --wait "did the migration land? one line"       # one bare reply; humans have no default deadline
rimz message @all --wait --json "status? one line"                   # one handle-keyed reply map
rimz message --all @reviewer --wait --any "first verdict?"           # return on the first terminal turn
rimz message @codex --wait=5m "open the PR"                          # total delivery + turn deadline
rimz message --steer @claude --wait "answer from this turn"         # the live turn's remainder is the reply
```

A one-agent text wait keeps the compact output: the final assistant message alone on stdout. A fan-out text wait streams labeled blocks in completion order; failed legs write forensics to stderr while the remaining legs keep gathering. `--json` buffers one uniform map for either arity: `{"@coder":{"status":"completed","reply":"landed","message_id":"msg_…"}}`; non-reply failures also carry `error`.

Agent-to-agent waits detect mutual and multi-agent reply cycles before they can hang: RimZ refuses or aborts the youngest wait, names the blocking handle and message, and leaves parked text queued to deliver at the next turn boundary. A bare agent-authored `--wait` has a one-hour default deadline as a backstop; use `--wait=<duration>` to choose another bound, while a human's bare wait remains indefinite.

The gathered join exits 0 only when every leg completes and otherwise uses the first non-completed leg's status in target order; a deadline exits 124 after classifying unfinished legs as `timed_out`. `--any` returns on the first terminal leg regardless of success or failure, emits only that winner, and leaves every other delivered message in flight. `Waiting` and `Paused` remain inside a reply turn, so use `--wait=<duration>` when a script needs a bound.

Every resolved target must be an existing lifecycle-bound agent with installed and trusted hooks. `--wait` conflicts with `--create`, `--schedule`, and `--no-enter`; `--json` and `--any` require `--wait`. Steering mid-turn treats the rest of that interrupted turn as the reply.

Every message is a record you can read back and steer after the fact:

```sh
rimz message list                       # the current channel's inbox, newest first
rimz message list --all                 # every channel, grouped by #channel
rimz message show msg_01k…              # full text, event timeline, and the first delivery blocker
rimz message edit msg_01k… --text "…"   # revise a still-queued message before it lands
rimz message steer msg_01k…             # push a queued record through now, skipping its schedule and gate
rimz message requeue msg_01k…           # send a terminal message again as a fresh record
rimz message remove msg_01k…            # drop a queued message
rimz message clear @codex               # drop every open message for one agent; targetless clears the channel
```

Statuses read straight across: `queued` and `claimed` are still live, `sent` means the bytes reached the pane, `delivered` means the prompt's turn started, and `archived` means the receiver or its channel ended. Use `message show` to diagnose a record that has not delivered. A durable file is the source of truth, so a missed notification or a crash between claim and send loses nothing.

Deliver a file's contents verbatim — a prompt with real newlines, no escaping — with `--file`:

```sh
rimz message @claude --file review-notes.md
```

Piped stdin is also verbatim and becomes the whole message when no inline text is present. Delivery waits for the pipe to close; when a pipe remains open without data for 10 seconds, RimZ reports the wait on stderr and continues waiting. Combine piped and inline text to put an instruction first and wrap the piped content in `<stdin>` tags; `--file` and piped stdin are mutually exclusive.

```sh
git diff main | rimz message @reviewer "review this"
```

## Agents message each other

`rimz message` is the same command whether you type it or an agent runs it, so a running agent hands work to a teammate exactly as you do. A delivery from another agent arrives prefixed `from @sender:` and lands as a first-class line in the receiving agent's transcript, so a channel's cross-talk reads as a conversation. `--no-from` delivers verbatim without the prefix when a script wants the raw text.

Read the conversation back — every prompt, answer, and inter-agent message across a channel or one agent, with each exchange grouped under the message that opened it — with `rimz transcript` ([transcript CLI](../reference/cli/transcript.md)).

## Channels

A channel groups the agents working one line of work inside a room: it is the identity the sidebar groups by, the `#channel` an address targets, and the tab name RimZ restores when it reopens the room. Your pane always sits in a channel, so `@codex` scopes to it by default and you only reach across when you name one.

Channels come from three places:

- **A Git worktree** — every agent working an isolated worktree shares that channel, named for the worktree ([Worktrees guide](./worktrees.md)).
- **A named channel** — a durable `#design`, `#ops`, or `#release` group with no worktree behind it.
- **A team or the room root** — a named team launched in place, or the plain room directory for agents with no other channel.

Create and manage named channels directly:

```sh
rimz channel new design                          # a durable #design channel, opened as a tab
rimz channel list                                # named, worktree, and live channels, with who's in each
rimz agents claude --channel design "draft it"   # launch straight into the channel
rimz message @planner#design --create "plan it"  # or reach into it, launching on miss
rimz channel rm design                           # remove a named-channel record
```

Named channels and worktrees share one namespace, so a name is a worktree channel or a named channel, never both. How channels render on screen — the pods, headers, and glyphs — is [the sidebar guide](./sidebar.md); the full command surface is [cli/channel.md](../reference/cli/channel.md).

## Asks and answers

An agent holding a permission prompt, plan approval, or user question reserves its input. `rimz asks` reads every open prompt as structured data, and `rimz answer` drives supported choices through the same native terminal interface you would use in the pane:

```sh
rimz asks --json
rimz asks show @planner
rimz answer @planner safe
rimz answer ask_0123456789abcdef 2
```

One answer command submits the whole ask. Positional selectors cover one choice per question and comma-separated multi-select choices; JSON input carries mixed choices and free text across several questions. The ask id is a compare-and-swap token, so a prompt already answered or superseded receives no stale keystrokes.

An answer differs from a message. A waiting prompt owns the agent's input and keeps ordinary messages parked; a supported answer enters the prompt UI, releases it, and confirms that lifecycle change before returning. Claude questions support picks, multi-select, and free text. A permission ask offers `allow` once, and a plan approval offers caution-marked `approve`, which enables auto-accept edits. Denial, persistent grants, keep-planning, refinement text, and manual-review approval stay in the Claude pane because their stable controls cannot provide the same confirmation contract.

## See also

- [Agents](./agents.md) — how handles and profiles come to be.
- [Teams](./teams.md) — how team roles become handles in a shared channel.
- [Worktrees](./worktrees.md) — the isolated tree behind every worktree channel.
- [The sidebar](./sidebar.md) — reading the channels and the messages they carry on screen.
- [Scripting agents](./scripting.md) — `--wait` and message records inside pipelines and CI.
- [Loops and schedules](./loops.md) — scheduled and handler-driven messages that steer the fleet unattended.
- [cli/message.md](../reference/cli/message.md) · [cli/asks.md](../reference/cli/asks.md) · [cli/channel.md](../reference/cli/channel.md) — the exact flags.
- [Message internals](../internals/harness/messaging.md) — the delivery engine underneath.
