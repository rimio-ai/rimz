# Message CLI

`rimz message` types text into a running agent's pane and keeps a durable record of every send, so you can inspect, edit, force, or cancel it until it lands. `rimz msg` is a visible alias for the command and all its subcommands. By default a message parks until the agent's current turn ends; `--steer` writes into the live turn now, and `--schedule` holds the message until a time. The [messaging guide](../../guide/messaging.md) teaches the workflow, and [message internals](../../internals/harness/messaging.md) explain the delivery engine.

```sh
rimz message @swift-otter "Add focused tests for the parser."        # park, or deliver now if the agent is free
rimz message --steer @claude "Inspect the failing test now."          # write into the live turn now
rimz message --on any @codex#cli-docs "If the run failed, capture the error first."
rimz message --schedule 14:30 @planner "Restart the review."
rimz message @coder --after @planner "Read plan.md when the planner finishes."
rimz message @codex --when '@codex idle 58m' "Keep the prompt cache warm."
rimz message @coder --wait "did the migration land? one line"
rimz message @all --wait --json "status? one line"
git diff main | rimz message @reviewer --stdin "Review this change."
rimz message                                                          # inbox for the current lane
rimz message show msg_01k…
rimz message edit msg_01k… --text "Use the cache key from config."
rimz message steer msg_01k…
rimz message cancel msg_01k… msg_01k…
rimz message clear @claude-2#cli-docs
```

## Send a message

A send takes one target and the text: `rimz message <TARGET> "<TEXT>"`. Flags may come before, between, or after them. Pass the text as one quoted argument; a text that starts with `-` goes after clap's `--` terminator.

The target follows the [address grammar](./agents.md#addressing-agents), plus `@me` for the calling agent. A bare word that is not an address fails: `rimz message msg_01k…` suggests `rimz message show msg_01k…`, a word carrying text to deliver names the sigil (``agent target `codex` must start with `@` (try `@codex`)``), and a word with nothing to deliver lists the subcommands. An address that matches no agent prints the error followed by the live agents, and exits 1.

The text comes from one of three sources:

| Source | How it is read |
| --- | --- |
| Inline argument | Trimmed. `\n` becomes a soft newline in the agent's composer and `\\` a literal backslash; every other backslash is kept, so `\d+` and `C:\tmp` arrive unchanged. |
| `--file <PATH>` | Sent as written, with no escape processing; only the trailing newline is dropped. Conflicts with inline text and `--stdin`. An empty file is refused. |
| `--stdin` | Read to EOF, with the surrounding whitespace trimmed and no escape processing. With inline text too, the inline text comes first and stdin follows inside `<stdin>` and `</stdin>` lines. Conflicts with `--file`. |

Piped stdin without `--stdin` is ignored, with a warning on stderr.

## When a message lands

Every send writes a `queued` record first. The mode decides when RimZ types it into the pane:

| Mode | Flag | Behaviour |
| --- | --- | --- |
| Park | default | Delivers now if the agent can take it, otherwise waits for the next turn boundary that `--on` allows. |
| Steer | `--steer` | Writes into the live turn now. It waits for any RimZ write already in progress to that pane. With no live pane yet, it parks the record. Conflicts with `--on`, `--schedule`, `--after`, and `--when`. |
| Interrupt | `--interrupt` | Types the text into the live turn, then presses Escape three seconds later so the agent stops that turn and submits the text as a fresh one. Claude Code and Codex only; other agents are refused. Conflicts with `--steer`, `--on`, `--schedule`, `--after`, and `--when`. |
| Schedule | `--schedule <DUR\|HH:MM>` | Always parks, and the record cannot deliver before that time. After it comes due, the park rules still apply. |

`--schedule` takes a duration with `s`, `m`, `h`, or `d` (`90s`, `60m`, `2h`, `1d`), or a 24-hour `HH:MM` time in the configured timezone. A time already past today means tomorrow. Zero is rejected. The room must be open, because its sidebar elder wakes due messages.

A parked message delivers once all of these hold, checked in this order:

1. Its `--schedule` time has passed, and every `--after` and `--when` condition is met.
2. It is the oldest ready message for that agent. Messages to one agent deliver oldest first; scheduled and condition-held messages step out of line until they are ready.
3. The agent is not compacting its context, and its status opens the `--on` gate: `idle`, `success`, or `sleeping` for `done` (the default), plus `failed` for `any`.
4. No open prompt in the agent's pane reserves its input, unless the message has `--force`.
5. A live pane can receive the text.

Parking needs the agent's hooks installed and trusted, because turn-end hooks trigger delivery; a send that would park for a kind without them is refused. `rimz message show <id>` names the first unmet condition.

Interrupt skips Escape when the agent is already resting. A native prompt waiting for input is refused unless `--force` is passed; the Escape then also cancels that prompt. A live pane without a durable session is refused; a durable session without a live pane parks the message. Unsupported agents, including any unsupported member of a fan-out, are refused before any message is recorded. `RIMZ_MESSAGE_INTERRUPT_DELAY_MS` sets the gap between the text and Escape, in milliseconds (default 3000).

Interrupt requires an existing recipient and refuses `--create`.

### Receipts

The send prints one line per target:

| Output | Meaning |
| --- | --- |
| `delivered to @coder (msg_…)` | Park mode found the agent free and typed the text. |
| `sent to @coder (msg_…)` | `--steer` typed the text. |
| `delivered to @coder (msg_…)` | `--interrupt` typed the text, then pressed Escape if the agent was mid-turn. |
| `queued for @coder (msg_…): delivery deferred; send now: rimz message interrupt msg_…` | Interrupt could not deliver, for example because the pane was absent. |
| `queued for @coder (msg_…) — @coder is running; send now: rimz message steer msg_…` | Parked because of the agent's status. |
| `queued for @coder (msg_…) — @coder is waiting on input in its pane; answer it or force: rimz message steer msg_… --force` | Parked behind an open prompt. |
| `queued for @coder (msg_…)` | Parked for a schedule, a condition, an older message, or a missing pane. |
| `compacted @coder` | Printed before the delivery line when `--smart-compact` fired. |
| `compacting @coder; queued msg_… (delivers when compaction completes)` | The compact command landed; the prompt follows once compaction ends. Steer prints `queued for @coder (msg_…; waiting for compaction)`. |

A status in a receipt comes from the agent's last recorded state and does not prove its pane is still live. A steer that reaches several agents prints one summary instead: `sent 2 agent(s): @claude (msg_…), @codex (msg_…); queued: …; compacted: …; waiting in pane: …`, with empty parts left out. A single `--steer` to an agent with an open prompt fails with `@coder (msg_…) is waiting on your input in its pane; answer it or pass --force`.

## Conditions

`--after` and `--when` hold a parked message until something happens to another agent. Both are repeatable, and every condition must be met. They combine with each other, `--schedule`, `--force`, and a fan-out recipient. Both conflict with `--steer`, `--wait`, and `--create`. `rimz message steer <id>` delivers past unmet conditions.

**`--after <ADDR>`** waits until that agent has finished its ready queued work: its status opens the message's `--on` gate and it has no undelivered message that is due. A failed turn keeps it waiting under `--on done` and releases it under `--on any`.

| Rule | Behaviour |
| --- | --- |
| Target | Exactly one existing agent with lifecycle state. A fan-out address, or the message's own recipient, is refused. |
| Already satisfied | An agent that is idle with no ready work meets the condition at send time, and it stays met. Queue the upstream work before the message that waits on it. |
| Missing agent | A referenced agent that is gone keeps the message waiting. |

**`--when '<ADDR> <STATUS> <DURATION>'`** waits until one agent has stayed in a status continuously for the duration, for example `--when '@coder running 2h'`.

| Rule | Behaviour |
| --- | --- |
| Target | Exactly one existing agent with lifecycle state. A broadcast is refused; the recipient itself is allowed, which makes keep-warm messages work. |
| Status | `running`, `waiting`, `idle`, `success`, or `failed`. These are raw statuses: a completed turn reads `success`, and `paused` is rejected. |
| Duration | `s`, `m`, `h`, or `d`; zero is rejected. |
| Once met | The condition stays met, even if the watched agent changes status before the recipient takes the message. |
| Watched agent ends | While the condition is unmet, the message is archived with the reason. |

## Targets and fan-out

An address that matches several agents is an error that lists the handles, unless the send opts into fan-out:

| Form | Reaches |
| --- | --- |
| `@all` | Every agent in the channel. Sent by an agent, it skips the sender (even with `--no-from`), and with no other agent it fails with `no other agents in the current channel`. Address the sender's exact handle to message itself. |
| `--all @codex` | Every agent the address matches, the sender included. |

Each fan-out delivery starts with the typed selector, such as `@all, ` or `@codex, `, so every recipient reads a group message. Deliveries go out one after another, and an agent that cannot take the message now does not stop the rest.

| Flag | Effect |
| --- | --- |
| `--channel <NAME>` | Match only agents in that named channel. Same as an inline `#NAME`. Conflicts with `--worktree`. |
| `--worktree <NAME>` | Match only agents in that worktree, by name or path. Conflicts with `--channel`. |
| `--create` | When the address matches no agent, launch one from a kind (`@codex`) or profile (`@planner`) with the text as its first prompt. `--worktree <NAME>` launches in that worktree; an inline `#NAME` or `--channel <NAME>` registers a named channel and launches there, and is refused when a worktree already owns the name. A pet name or ordinal cannot create. Conflicts with `--schedule`, `--after`, `--when`, and `--wait`. |

## Delivery options

| Flag | Effect |
| --- | --- |
| `--on done\|any` | Which turn outcomes release a parked message. Default `done`. |
| `--force` | Deliver even while an open prompt in the agent's pane reserves its input. |
| `--no-enter` | Type the text and leave it unsubmitted in the composer. |
| `--smart-compact <PCT\|TOKENS>` | When the agent's context is at least this full, send its compact command first and the message after compaction. Takes a percentage (`70%`, at most 100) or an occupied-token count (`120000`, `180k`, `1m`). A context below the threshold sends the message alone. Unset, [`[harness] smart_compact`](../../guide/configuration.md#smart-compaction) applies, and `[harness] compact_instruction` rides agents whose compact command accepts guidance. |
| `--no-from` | Send the text with no message header. |

## The message header

Unless `--no-from` is set, every delivery starts with a three-line header before the text:

```text
Type: AGENT_MESSAGE
From: @swift-otter (planner)
Content:
Read plan.md when the planner finishes.
```

| `Type` | Sent for | `From` |
| --- | --- | --- |
| `USER_MESSAGE` | A send from a user shell | `@user` |
| `AGENT_MESSAGE` | A send from an agent, whether RimZ launched it or only saw it running | Reply address: role, else explicit name, else pet name; kind only when no role or name exists. Adds `#channel` when crossing channels, then the profile (else kind) in parentheses unless it repeats the handle base. |
| `SUBAGENT_REPORT` | The status digest sent once all of an agent's [subagents](./subagents.md) and background runs settle | `@rimz` |
| `WAIT` | A [`rimz wait`](./wait.md) delivery, or a loop `--wait` delivery not triggered by a signal | `@rimz` |
| `SIGNAL` | A [signal-triggered loop](./loop.md#signals) delivery | `@rimz` |
| `STAGE` | A [team stage](./teams.md) opening | `@rimz` |

`rimz transcript` hides `@rimz` deliveries from its human view; `rimz transcript --json` keeps them.

## Wait for replies

With `--interrupt --wait`, the turn the prompt starts is the reply. RimZ waits for that prompt's delivery acknowledgement before following the reply, not the interrupted turn's remaining activity.

`--wait` sends or parks one message per target, waits for each reply turn to finish, and prints the replies. It takes an optional total deadline in the attached form `--wait=<DURATION>` (`s`, `m`, or `h`), covering delivery plus the turn. Bare `--wait` takes no value, so it can sit before the text. A user shell's bare wait has no deadline; an agent's bare wait stops after one hour.

| Flag | Effect |
| --- | --- |
| `--json` | Print one map keyed by agent handle after the join settles. Requires `--wait`. |
| `--any` | Return when the first reply turn ends, successful or not, and print only that reply. The other messages stay in flight. Requires `--wait`. |

Text output for one target is the reply alone. For several targets, each reply prints under an `@handle:` line in completion order, and a failed target writes its failure, with the transcript path when there is one, to stderr without stopping the others. Replies render by the [agent-prose rule](../cli.md#agent-prose). A turn that is `waiting` or `paused` is still in progress, so a script that needs a bound passes a deadline. With `--steer --wait`, the rest of the live turn is the reply.

Each `--json` value carries `status`, `reply` (a string or `null`), and `message_id`, plus `error` when delivery failed, the agent stopped, or the wait aborted:

```json
{"@coder":{"status":"completed","reply":"landed","message_id":"msg_…"}}
```

The exit code is the first non-completed target's run status, in target order:

| Code | Meaning |
| --- | --- |
| `0` | Every reply turn completed. |
| `1` | A turn failed, or the wait could not run. |
| `123` | Verification failed. |
| `124` | The deadline passed. Unfinished targets read `timed_out`, and `--json` still prints the map. |
| `125` | A budget cap stopped the turn. |
| `130` | Canceled. |

A wait is refused before sending when a target has no lifecycle state, is not running, or lacks installed and trusted hooks. It is also refused with `--create`, `--schedule`, or `--no-enter`. A wait that would deadlock, because the target is waiting on the caller's own reply, is refused or aborted with the blocking handle and message named; any parked text stays queued. A single target holding an open prompt fails with the `waiting on your input` error.

## Message statuses

| Status | Meaning |
| --- | --- |
| `queued` | Waiting to deliver. Open: editable, steerable, cancelable. |
| `claimed` | A delivery is writing it right now. Open, but edit and steer refuse it. |
| `sent` | RimZ typed it into the pane; the agent has not acknowledged it yet. |
| `delivered` | The agent acknowledged it: a turn started with the prompt, or compaction started for a command. |
| `timed_out` | The agent never acknowledged it. A command is never resent, because repeating `/compact` can discard context. |
| `errored` | Delivery could not happen, for example the address matched no agent. |
| `canceled` | `message cancel` or `message clear` stopped it. |
| `abandoned` | Delivery attempts failed repeatedly before anything reached the pane. |
| `archived` | The recipient, its channel, or a watched `--when` agent ended first. |

The last six are final. A final record keeps its text in `audit/messages/<bucket-start>.jsonl`, subject to the room's audit retention. `show`, `list`, and `requeue` read the room's newest 500 final records, so they display or resend a final message while it is among those and still retained.

## Inbox and queue verbs

`rimz message interrupt <id> [--force]` pushes a queued record with the interrupt policy above, overriding its delivery gate and conditions. Claimed and finished records are refused, as with `steer <id>`. The mode is selected at push time, not stored on the record; a parked interrupt follows ordinary delivery at the next boundary, without Escape.

### List messages

Bare `rimz message` is `rimz message list` for the current lane. In the main checkout, which has no lane, it shows messages without a channel.

| Flag | Effect |
| --- | --- |
| `[TARGET]` | Only messages to that agent. |
| `--all` | Every lane, including archived messages. |
| `--channel <NAME>` | One lane. |
| `--status <STATUS>` | Exactly one status. Also accepts `pending`, `cancelled`, and `removed`. Selecting `archived` shows archived messages. |
| `--system` | Include system traffic: waits, signals, subagent digests, stage notices, compaction commands, and `--no-from` text. |
| `--limit <N>` | At most N rows, newest first. Default 200; `0` shows all. |
| `--json` | Emit the rows as a JSON array. |

The list shows conversation by default: sends from you and from agents. Archived messages are hidden unless `--all` or `--status archived` asks for them. Each row is two lines, the header then the text clipped to the terminal width with `$HOME` shown as `~`:

```text
you → @coder  queued  3m ago  msg_01k…
  Read plan.md when the planner finishes. · after @planner
```

The sender reads `you` for a user shell, the handle for an agent, `@rimz` for RimZ notices, and `rimz` for `--no-from` text. A handle drops `#channel` when the list is already scoped to that channel. A final message whose text was not retained shows its terminal reason instead. With `--all`, rows group under a `#channel` or `(main)` header, lanes with the newest message first.

Trailing lines report what the view left out:

| Line | When |
| --- | --- |
| `... N older messages hidden (--limit 0 for all)` | The limit cut rows. |
| `... N system messages hidden (--system shows them)` | System messages matched the lane, status, and target filters. Printed even when no conversation rows remain. |
| `no messages in #cli-docs — rimz message list --all shows every channel` | Nothing matched. It reads `no messages in the main lane` outside a lane, bare `no messages` with `--all`, and names the status with `--status` (`no queued messages …`). |

`--json` applies the same filters and prints no hidden counts. Each row carries `message_id`, `kind`, `agent_id`, `sender`, `body` (`prompt` or `command`), `enter`, `gate`, `force`, `status`, `enqueued_at`, `updated_at`, `attempts`, and `unconfirmed_sends`. When set, a row adds `address` (the recipient's handle at send time), `agent_name`, `channel`, `text`, `pane_id`, `last_attempt_at`, `last_error`, `delivered_at`, `not_before`, `after`, `when`, `retry_after`, `auto_compact`, and `compacted_context_tokens`.

### Show one message

`rimz message show <id>` (alias `status`) prints the record's fields, its full text by the [agent-prose rule](../cli.md#agent-prose), and its event timeline. For an open message it adds a `DELIVERY CHECK` with one row each for `schedule`, `after`, `when`, `fifo`, `agent`, `gate`, `ask`, and `pane`, a verdict naming the first blocker, and the command that clears it when there is one:

```text
DELIVERY CHECK
  …
  waiting: @coder is running; gate 'done' opens at next turn end
  force now: rimz message steer msg_01k…
```

A scheduled message suggests `rimz message edit <id> --no-schedule` as well, and an open prompt suggests `steer <id> --force`. `--json` prints `{"message": <row>, "timeline": [{"method", "at", "attempts", "reason"}], "delivery": {"check", "verdict"}}`, with `delivery` present only for open messages. Every sender class is reachable, including system messages the list hides.

### Change a queued message

| Command | Accepts | Prints |
| --- | --- | --- |
| `message edit <id>` | A `queued` message | `edited msg_… (<fields>)` |
| `message steer <id> [--force]` | A `queued` message | `sent to @coder (msg_…)` |
| `message requeue <id>` | A final message whose text was retained | `queued for @coder (<new id>)  (from <old id>)` |
| `message cancel <id>...` (alias `remove`) | Open messages | `canceled msg_…` or `msg_… cannot be canceled`, per id |
| `message clear [TARGET]` | Every open message for one agent, or in a lane | `canceled N message(s) for @coder: msg_…, …` |

`edit` changes only how a message delivers. The recipient, channel, and sender are fixed, so retarget by canceling and sending again. `edit` and `requeue` take the same flags, and `edit` with none is refused:

| Flag | Changes |
| --- | --- |
| `--text <TEXT>`, `--file <PATH>` | The text. The two conflict. |
| `--on done\|any` | The gate. |
| `--schedule <DUR\|HH:MM>`, `--no-schedule` | Set or clear the earliest delivery time. |
| `--force`, `--no-force` | Whether an open prompt blocks delivery. |
| `--enter`, `--no-enter` | Whether the text is submitted. |
| `--smart-compact <PCT\|TOKENS>`, `--no-smart-compact` | Set or clear the compaction threshold. |

`steer` delivers now, past the schedule, conditions, queue order, and gate. An open prompt still blocks it without `--force`, and a missing agent or pane fails with the reason. A `claimed` message is refused with `delivery in progress; retry in a moment`, and a final one points to `requeue`.

`requeue` writes a new queued message with a new id. It keeps the recipient and delivery settings unless flags override them, and re-arms every `--after` and `--when` condition. A message that is still queued is refused with a pointer to `edit` or `steer`, and a `sent` one must settle first.

`cancel` processes every id, then exits 1 if any was not open. `clear` with a target cancels that agent's open messages. Without one, it cancels open messages in the lane from `--channel`, `--worktree`, or the current channel, and fails when there is none. Both reach system messages the list hides, and canceled records keep their text for `requeue`.
