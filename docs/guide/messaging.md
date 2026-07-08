# Messaging

`rimz message` types text into a running agent's own pane, the same way you would. It reads like Slack — every agent answers to a handle — and it guarantees the text lands: parked by default (held until the agent's current turn finishes), right now with `--steer`, or at a wall-clock time with `--schedule`.

The same command serves you, your scripts, and the agents themselves — agents talk to each other through it too. One prompt reaches one teammate; `@all` reaches everyone in your channel.

```sh
rimz message @claude "add coverage for the expiry edge cases"      # parks: lands when @claude's turn ends
rimz message --steer @claude "stop: the parser test comes first"   # interrupts the live turn now
rimz message --schedule 60m @codex#feat-b "run the smoke test"     # lands in an hour
rimz message @all "summarize what changed at the next boundary"    # everyone in the current channel
```

## Address an agent

An address is `@handle[#channel]`. The handle names who; the optional `#channel` names which group of agents — every worktree gets a channel automatically, and named channels group agents across them ([Channels below](#channels)). Leave it off and Rimz uses the channel your pane is in.

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
- A live pane exists to receive it, and the agent's reporting hooks are installed, since hooks are how Rimz learns the turn ended.

`rimz message show msg_…` names the first unmet condition when a message is still waiting, so you never have to guess why.

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

Give a percentage of the window (`70%`) or an occupied-token count (`120000`). Omit the flag and Rimz uses the `[harness] smart_compact` default from your config — set it once and every message inherits the behavior ([configuration → smart compaction](../reference/configuration.md#smart-compaction), [setup guide](./setup.md)).

## Confirm, inspect, and fix delivery

Sending returns immediately. To block until the agent acknowledges the text, add `--wait`:

```sh
rimz message --steer @claude --wait "run the smoke test"      # returns when Claude confirms, nonzero on timeout
rimz message @codex --wait=5m "open the PR"                   # give it up to five minutes
```

`--wait` exits nonzero on anything but a confirmed delivery, which is what a script branches on ([scripting guide](./scripting.md)).

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

Statuses read straight across: `queued` and `claimed` are still live, `sent` means the bytes reached the pane, `delivered` means the agent acknowledged them, and `archived` means the receiver or its channel ended. A durable file is the source of truth, so a missed notification or a crash between claim and send loses nothing.

Deliver a file's contents verbatim — a prompt with real newlines, no escaping — with `--file`:

```sh
rimz message @claude --file review-notes.md
```

## Agents message each other

`rimz message` is the same command whether you type it or an agent runs it, so a running agent hands work to a teammate exactly as you do. A delivery from another agent arrives prefixed `from @sender:` and lands as a first-class line in the receiving agent's transcript, so a channel's cross-talk reads as a conversation. `--no-from` delivers verbatim without the prefix when a script wants the raw text.

Read the conversation back — every prompt, answer, and inter-agent message across a channel or one agent — with `rimz transcript` ([transcript CLI](../reference/cli/transcript.md)).

## Channels

A channel groups the agents working one line of work inside a room: it is the identity the sidebar groups by, the `#channel` an address targets, and the tab name Rimz restores when it reopens the room. Your pane always sits in a channel, so `@codex` scopes to it by default and you only reach across when you name one.

Channels come from three places:

- **A Git worktree** — every agent working an isolated worktree shares that channel, named for the worktree ([agents guide → worktrees](./agents.md)).
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

## See also

- [Agents, worktrees, and teams](./agents.md) — how handles, profiles, roles, and worktree channels come to be.
- [The sidebar](./sidebar.md) — reading the channels and the messages they carry on screen.
- [Scripting agents](./scripting.md) — `--wait` and message records inside pipelines and CI.
- [Loops and hands-off operation](./loops.md) — scheduled and handler-driven messages that steer the fleet unattended.
- [cli/message.md](../reference/cli/message.md) · [cli/channel.md](../reference/cli/channel.md) — the exact flags.
- [Message internals](../internals/harness/messaging.md) — the delivery engine underneath.
