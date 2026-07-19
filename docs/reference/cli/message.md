# Message CLI

`rimz message` routes text to a running agent through the same path a keystroke takes: it types into the agent's own pane, exactly as if you typed there, and records the message durably so delivery is auditable and reversible. `rimz msg` is a visible alias for the whole command, including its subcommands. The default parks text for the next safe turn boundary, sending immediately only when the agent is already open to receive; `--steer` interrupts the live pane now; `--schedule` sets the earliest delivery time before the usual `--on` gate opens. A parked message is a `queued` record you can inspect, edit, or cancel before it lands. Addresses, park-vs-steer, channels, and agent-to-agent chat as a concept are the [messaging guide](../../guide/messaging.md).

```sh
rimz message @swift-otter "Add focused tests for the parser."                   # park or send now if open
rimz message --on any @codex#cli-docs "If the run failed, capture the error first."
rimz message --schedule 60m @claude "Run the smoke test after lunch."
rimz message --schedule 14:30 --on any @planner "Restart the review."
rimz message @coder --after @planner "Read plan.md when the planner finishes."
rimz message @codex --when '@codex idle 58m' "Keep the prompt cache warm."
rimz message --steer @claude "Inspect the failing test now."
rimz message @coder --wait "did the migration land? one line"
rimz message @all --wait --json "status? one line"
rimz message --steer @codex --no-enter "Use the docs branch only."              # paste, don't submit
rimz message --steer @planner --create "Draft the new endpoint."                # launch if missing
rimz message @all "When you reach a boundary, summarize what changed."
git diff main | rimz message @reviewer --stdin "Review this change."
rimz message                                                                  # inbox for the current lane
rimz message list --json
rimz message list --channel cli-docs --status queued
rimz message show msg_01k…                                                    # status alias kept
rimz message edit msg_01k… --text "Use the cache key from config."
rimz message edit msg_01k… --no-schedule                                      # clear an earliest-delivery floor
rimz message steer msg_01k…                                                   # force a queued record now
rimz message requeue msg_01k… --text "Retry with this narrower ask."
rimz message cancel msg_01k… msg_01k…
rimz message clear                                                            # clear open messages in the current lane
rimz message clear @claude-2#cli-docs
```

The message is one bare quoted argument, so no `--` separates ordinary prose from flags. A message that starts with `-` still uses clap's universal terminator (`--`) before the text. A wait duration uses the attached form `--wait=<duration>`; bare `--wait` has no value and can precede the message.

Address the target with the [agent-address grammar](./agents.md#addressing-agents). A fan-out tags each delivery with the addressed handle, and an unmatched address prints the live-agent list.

## Send modes

`--steer` delivers fresh text to live panes immediately, writes a durable prompt record, and prints `sent to @handle (msg_...)`; smart compaction adds a durable command record and `compacted @handle` before the prompt line. An agent Waiting on a native ask still reserves input unless `--force` is passed, and a missing receiver or pane is a hard error. Broadcasts summarize sent and skipped agents with handles and message ids, so one blocked agent never stops the rest.

The default mode uses the same live path when the addressed agent can receive now: a live pane exists, the `--on` gate is open, no pending ask reserves input unless `--force`, and no older ready message owns that card's FIFO head. Otherwise it parks a `queued` prompt record until `--on done` (idle or success) or `--on any` (idle, success, or failed) opens.

`--schedule <DUR|HH:MM>` always parks and sets a `not_before` time floor; examples include `90s`, `60m`, `2h`, `1d`, and configured-timezone 24-hour times such as `14:30`. A scheduled message becomes eligible only after that floor, then the normal gate and pending-ask checks still apply.

`--after <ADDR>` holds delivery until exactly one referenced agent reaches the message's `--on` gate with no schedule-ready undelivered work. Repeat it to require several agents. Queue upstream work first: an already idle, quiescent reference is stamped satisfied at enqueue and remains satisfied. Unmet conditions step out of the receiver FIFO, and `message steer <id>` bypasses them. A referenced failure waits under `--on done` and releases under `--on any`; a missing referenced card keeps waiting. `--after` conflicts with `--steer` and `--wait`, requires an existing recipient instead of `--create`, rejects fan-out references and the message recipient itself, and composes with `--schedule`, recipient fan-out, and `--force`.

`--when '@handle <status> <duration>'` holds delivery until exactly one existing lifecycle-bound agent stays continuously in `running`, `waiting`, `idle`, `success`, or `failed` for the duration. Durations use `s`, `m`, `h`, or `d`. Repeat it for an all-of set; it composes with `--schedule`, `--after`, recipient fan-out, and self-reference. It conflicts with `--steer`, `--wait`, and `--create`, rejects broadcast watched addresses and `paused`, and archives the whole record with an expiry reason if an unmet watched session ends. A met condition latches durably across later status changes.

A bare `@<kind>`, `@<profile>`, or `@all` in `--steer` mode also reaches an agent you just started in a fresh pane, before its first turn, because the live-pane side addresses the pane it types into. Parked records key on the bound session or launch placeholder card, so FIFO survives registration.

## Delivery flags

The flags worth knowing tune delivery (run `rimz message --help` for the full surface):

- `--steer` interrupts the live pane now and conflicts with `--schedule` and `--on`, because it has no later boundary.
- `--schedule <DUR|HH:MM>` sets the earliest delivery time for parked records; the room must be open so the sidebar elder can spawn the scheduled-wake helper when the stamp comes due.
- `--after <ADDR>` waits for another agent to finish its ready queued work; repeat it to wait for all named agents.
- `--when '@handle <status> <duration>'` waits for one agent's continuous raw-status dwell; repeat it to wait for all conditions.
- `--on done|any` chooses which turn-boundary statuses release parked records; `done` is the default.
- `--no-enter` pastes the text without submitting; otherwise the text rides as a bracketed paste and Enter lands as a discrete keystroke, so a `\n` in the text stays a soft composer newline and a multi-line prompt lands multi-line (write `\\` for a literal backslash).
- `--file <PATH>` reads the prompt from a file and sends it byte-for-byte: real newlines stay soft breaks and backslashes stay literal, so code and regex paste unchanged. It conflicts with inline text and `--stdin`.
- `--stdin` reads stdin verbatim to EOF. With inline text, RimZ puts that instruction first and wraps the stdin content in `<stdin>` tags; it conflicts with `--file`.
- `--channel <NAME>` scopes the target to a named channel; inline `#NAME` is the address form. `--worktree <NAME>` scopes to a worktree name or path.
- `--create` launches a missing agent from a kind or profile address with the text as its first prompt; inline `#NAME` or `--channel NAME` registers a named channel, while `--worktree NAME` creates or reuses Git backing.
- `--force` sends over a pending native ask; without it the ask keeps the next input reserved.
- `--smart-compact <PCT|TOKENS>` sends a tracked `/compact` command first when the agent's context window has reached the threshold (a percentage like `70%` or an occupied-token count like `120000` or `180k`), then sends the prompt one message interval later so it lands against a fresh window. Unset, [`[harness] smart_compact`](../../guide/configuration.md#smart-compaction) supplies the threshold; a window below it sends untouched.
- `--no-from` sends the bytes exactly. By default a RimZ-launched agent's send arrives as `from @sender: text`, gaining `#channel` when it crosses channels.
- `--wait[=DURATION]` sends or parks one prompt per resolved target and joins their reply turns. One text reply stays bare; fan-out replies are labeled and stream in completion order while failures write forensics to stderr without stopping the other legs. The gathered exit is 0 only when every leg completes, otherwise the first non-completed leg's exit code in target order; the total delivery-plus-turn deadline classifies unfinished legs as `timed_out` and exits 124. Each target needs an existing lifecycle-bound card with installed and trusted hooks. A human's bare `--wait` has no deadline; an agent caller's bare wait defaults to one hour, and `--wait=<duration>` overrides either behavior. Mutual agent reply waits are refused or aborted with the blocking handle and message named while parked text stays queued for the next boundary. `Waiting` and `Paused` keep blocking; `--steer --wait` treats the remainder of each live turn as the reply. It conflicts with `--create`, `--schedule`, and `--no-enter`.
- `--json` with `--wait` buffers one map keyed by canonical agent handle for both single and fan-out waits. Each value carries `status`, `reply` (string or `null`), and `message_id`; delivery failure, a stopped agent, or skipped Waiting input adds `error`.
- `--any` with `--wait` returns on the first terminal reply leg regardless of success or failure, emits only that winner, and exits with its status. The other messages stay in flight and are not canceled.

## The message record

Every send is a durable record, which is what makes delivery inspectable and reversible: target, channel, receiver card, and sender attribution are record identity, so you retarget by canceling and resending. `message edit <id>` changes only delivery fields on a `queued` record: text, `--on`, schedule or `--no-schedule`, `--force` or `--no-force`, `--enter` or `--no-enter`, and smart-compaction settings.

Message statuses are `queued`, `claimed`, `sent`, `delivered`, `timed_out`, `errored`, `canceled`, `abandoned`, and `archived`. `sent` means RimZ wrote the bytes to the pane; `delivered` means the agent acknowledged a prompt through `TurnStarted` or a command through `Compacting`; `archived` means the receiver or channel context ended.

The `message.sent` audit event records message id, receiver, pane, force flag, sender, body, and text length; message content stays in the message record. Terminal records keep their text in `messages/history.jsonl`.

## Inbox and queue verbs

Bare `rimz message` renders the inbox for the current lane; in the main checkout, where there is no stamped lane, it shows only lane-less messages.

`message list` hides archived records unless `--all` or `--status archived` asks for them, sorts newest first, and caps at 200 rows (`--limit N`, `--limit 0` for all). An empty result renders as a faint scope-aware `no messages` line; non-empty human output is a dense two-line digest: `sender → receiver  status  age  msg_id`, then an indented one-line snippet clipped to the terminal width after `$HOME` path segments collapse to `~`. Handles omit `#channel` when the row already sits inside that scoped lane, and terminal rows read their preserved text from `messages/history.jsonl` (older event-only rows show the terminal reason as the snippet). `--all` widens the view to every lane, groups rows under one `#channel` or `(main)` header per lane ordered by latest activity, and keeps messages newest-first inside each group; `--channel <NAME>` selects one lane, `--status <STATUS>` filters exactly, and `--json` keeps the full record including attempts and the enqueue-time receiver `address`.

`message show <id>` (`status` alias) prints the full record text, a `message.*` event timeline, and, for open records, a delivery check that names the first blocker: schedule floor, referenced agent, FIFO head, receiver presence, gate, pending ask, or live pane, followed by a steer or edit hint when a command can clear the blocker.

`message steer <id>` takes an existing queued record and sends it now, skipping its schedule floor, FIFO position, and turn-boundary gate; an agent Waiting on a native ask still reserves input unless `--force` is passed, and a missing receiver or pane stays a hard error.

`message requeue <id>` creates a new queued record from a terminal record whose text is still in `messages/history.jsonl`; it preserves receiver identity and delivery settings unless edit flags override them, and re-arms every `--after` and `--when` condition.

`message cancel` accepts one or more ids and keeps processing after misses, then exits non-zero if any id was not open. The `remove` alias stays available. A canceled record keeps its text in `messages/history.jsonl`, so `message show` can inspect it and `message requeue` can revive it. `message clear` with a target cancels that agent's open messages; without a target it cancels open messages in the scoped lane from `--channel`, `--worktree`, or the ambient room channel, and prints the ids it canceled.

Parked delivery needs installed and trusted hooks, because turn-end hooks trigger delivery of parked records; scheduled wakeups need an open room so an elder keeps time. The record layout, gates, and delivery walk are in [messaging.md](../../internals/harness/messaging.md).
