# Transcript CLI

`rimz transcript` prints a channel or one agent as a timestamped chat log, read from RimZ's own transcript log. RimZ records every prompt, answer, and inter-agent message itself, so the log still reads after an agent has ended and its provider's native session files have rotated away. The command only reads; it sends nothing to any agent. Reading a conversation back as part of everyday work is covered in the [messaging guide](../../guide/messaging.md), and the log itself (entry kinds, storage, retention) in [internals: the transcript](../../internals/harness/transcript.md).

```sh
rimz transcript                          # the current channel
rimz transcript '#cli-docs'              # one channel
rimz transcript @codex#cli-docs -n 4     # one agent, last 4 entries
rimz transcript run_0123456789abcdef0123456789abcdef   # a supervised run's agent
rimz transcript @all                     # every channel in the workspace
rimz transcript --all                    # include prior sessions
rimz transcript --flat                   # timestamp order, no threads
rimz transcript --json > /tmp/chat.json
```

## Choose what to read

The target names a channel or one agent, using the [agent-address grammar](./agents.md#addressing-agents).

| Target | Reads |
| --- | --- |
| none | The current channel. Outside a channel, every channel in the workspace. |
| `'#<channel>'` | That channel. |
| `@all` | Every channel in the workspace, with each handle suffixed by its channel. |
| `@all#<channel>` | That channel. |
| `@<handle>` | One agent, looked up in the current channel. |
| `@<handle>#<channel>` | One agent, looked up in that channel. |
| a session id | That agent session, in whatever channel it ran. |
| `run_<id>` | The agent session bound to that supervised run. Fails with `run <id> has not bound an agent session yet` until the run's agent registers. |

`-w <channel>` sets the channel for a target that names none. A target whose `#channel` disagrees with `-w` is an error.

Quote a target that starts with `#`. An unquoted `#` starts a comment in bash, and in zsh with `interactivecomments`, so `rimz transcript #cli-docs` runs as `rimz transcript` and shows the current channel.

### Channel views

A channel view merges the transcript of every root agent in the channel into one chat log, headed by a `#<channel>` line. Children launched through `rimz subagents` are not root agents: their conversations stay out of channel and `@all` views, in human and `--json` output alike. Read a child by targeting it.

### One agent

An agent view shows the messages the agent sent or received, including its messages as recorded in its peers' logs. Messages between agents in other channels are outside the view.

The target resolves to exactly one agent session, in this order:

1. A `run_<id>` resolves to the run's bound session.
2. An exact session id wins, in any channel.
3. Otherwise the selector matches handle, provider kind, name, profile, role, or session-id prefix within the channel. A live root agent in the room wins over ended sessions; among the remaining matches, the one with the latest transcript activity wins.

No match fails with ``no agent matches target `<target>` in the transcript log``.

Targeting a launched child shows the child's own conversation, with its launch brief attributed to the parent as `@parent → @child`. The parent's view leaves its children's conversations out.

### Channels in other workspaces

A channel named explicitly (`'#<channel>'`, `@<handle>#<channel>`, `@all#<channel>`, or `-w <channel>`) is looked up in the current workspace first. When the current workspace has neither transcript entries nor a live agent for it, RimZ searches every known workspace whose project root still exists, so the conversation reads from any directory. When several workspaces match, the command fails:

```text
#cli-docs has conversations in several workspaces: /home/me/api, /home/me/web; run from one of them or pass --root <path>
```

`--root <path>` pins the workspace and skips the search.

## Flags

| Flag | Effect |
| --- | --- |
| `[TARGET]` | What to read; see [Choose what to read](#choose-what-to-read). |
| `-w, --worktree <channel>` | Channel used to resolve the target. |
| `-n, --last <N>` | Keep the last `N` entries; see [Tail with `--last`](#tail-with---last). |
| `--all` | Include prior sessions; see [Prior sessions](#prior-sessions). |
| `--flat` | Print entries in timestamp order instead of grouping replies into threads. |
| `--json` | Emit JSON; see [JSON output](#json-output). |

`--color`, `--root`, and the backend flags are [global flags](../cli.md#global-flags).

## Prior sessions

By default a view starts at the current live cohort: the earliest registration among the live root agents in scope (or the targeted agent, for an agent view). After a same-name team or worktree relaunch, the view opens on the living conversation. Earlier lines stay in the log, and a note on stderr counts them:

```text
⋯ 12 earlier lines from a prior session (Fri, Sep 11 2026) — rimz transcript --all
```

`--all` prints the earlier lines first, then a `Live · <when>` rule before the current cohort's lines. When nothing in scope is live, or every line predates the cohort, the whole scope prints without a rule.

## Read the human view

This is a piped channel view: a user's prompt with the agent's reply threaded beneath it, then a hand-off that opens its own exchange.

```text
#cli-docs

 user  → @codex  14:02
Draft the transcript reference from the code.
│
│ @codex  14:09
│ Drafted docs/reference/cli/transcript.md. Asking @claude to review it.

@codex → @claude  14:09
Review docs/reference/cli/transcript.md against the transcript CLI code.
│
│ @claude  14:15
│ Two drifts: @all reads every channel, and --json drops focus for channel targets.
```

### Headers and grouping

Each header names the sender, then `→` and the receiver when there is one, then the local time as `HH:MM`. Human senders show as the `user` or `you` chip, and RimZ itself as `rimz`. A channel view drops the `#channel` suffix from handles. Consecutive entries from the same sender to the same receiver share one header when they fall within 5 minutes of each other, on the same day, in the same margin or thread. A rule labelled `Today` or with the date (`Sat, Jun 27 2026`) marks a change of day.

A message's time is its creation time when known, otherwise its recorded time. When delivery trails creation by at least 60 seconds, the header reads `12:41 · delivered 12:45`. Delivery on a different local date includes that date, as in `23:58 · delivered Mon, Jun 29 2026 · 00:02`. Such a line gets its own header, and the next line cannot share it. Older entries without a recorded creation time keep their original timestamp.

### Threads

Each conversation starts at the margin, with replies behind a `│` spine one level deep. A reply back to the sender continues the exchange only while it is the latest conversation; once another conversation's message, prompt, or flip intervenes, it opens at the margin. A hand-off to a third agent also opens its own exchange, as does a reply whose parent is outside the view. Messages, prompts, and flips keep their timestamp order; turn output sits beneath the message that opened its turn, however late it ran. When one turn answers two opening messages, its output sits under the later opener without merging their threads. A thread entry on a different day from its first line shows the date in its header (`Mon, Jun 29 2026 · 00:02`).

Output with no recorded parent, such as the reply to a prompt typed directly into the agent's pane, threads beneath that agent's most recently arrived opening message at the time of the output. A message still waiting for delivery cannot open that turn. Output stays at the margin when that opener is hidden (see [What the human view hides](#what-the-human-view-hides)). `--flat` turns threading off and uses timestamp order (`at` in JSON).

### Bodies

On a styled terminal, bodies render markdown (headings, emphasis, links, lists, code, quotes, tables) wrapped to the margin or thread width. Piped output and `--color never` print the markdown as written; `--color always` forces rendering. `@agent` and `#channel` mentions are highlighted, and provider API errors print as the agent's lines in the error tone.

### Asks

A blocking question or plan approval prints as a card behind its own `│` spine: the question, each option as `○ <label>` with its description beneath, and the answer folded in as `● <label> — you`. A card with no recorded answer ends in `◌ unanswered`. A prompt the user submits while the ask is open counts as its answer and folds into the card. An ask continues the agent's preceding block on the same day even after the 5-minute window.

### Stage flips

A [`rimz teams flip`](./teams.md#flip-the-board-to-the-next-stage) prints inside the channel's conversation as a flip line:

```text
@claude  14:10
⇢ Explore → Plan  notes written
⇢ Plan → Implement  14:13
```

The transition prints in the accent tone and the note faint, with further note lines hanging beneath. A flip continues its flipper's most recent line in the same channel when that line is within 5 minutes and belongs to the latest conversation, adding its own `HH:MM`; otherwise it opens a block under its own header. A flip with no prior stage reads `⇢ Plan · opened`, and a flip to the current stage reads `⇢ Plan · re-opened`. Flips come from the [`team.stage` signal](./teams.md#the-teamstage-signal) in the active event log, so flips older than the last event-log rotation do not print.

## What the human view hides

The human view leaves out RimZ's own deliveries, so a conversation shows the work and not the wakeups that resumed it. `--json` keeps every one of them.

| Hidden entry | Source |
| --- | --- |
| Fleet digest | The status-only `SUBAGENT_REPORT` from `@rimz` when an agent's launched subagents settle. |
| Wait | A [`rimz wait`](./wait.md) or loop `--wait` delivery, a signal delivery, or a stage notice, from `@rimz`. |
| `rimz` prompt | A prompt RimZ sent without a header, such as a nudge or a confirmed `rimz message --no-from` send. |

Each hidden entry takes with it the agent's replies and error output in the turn it opened. Three things stay visible: an ask raised in that turn, the output of a turn that asked the user, and any message the agent sends. A turn opened by both a hidden entry and a visible message stays visible. No flag shows hidden entries in the human view.

## Tail with `--last`

`-n, --last <N>` keeps the last `N` entries in display order, after hidden entries are removed. One entry is one message, however many lines its body takes, and an answer folded into an ask card still counts as one. When the cut lands inside a thread, the tail widens back to the displayed thread's first line so the conversation keeps its opener. `--last` applies to `--json` the same way.

## Empty results

A scope with nothing to show exits 0 and prints a faint note on stderr. It reads `No conversation for #<channel> yet.` when the scope has a channel, `No conversation for <target> yet.` or `No conversation for this room yet.` otherwise, and `No conversation recorded yet.` when the log is empty or `--last 0` trimmed everything. With `--json`, stdout gets `{"entries": []}`.

## JSON output

`--json` prints one object with the selected entries in timestamp (`at`) order, hidden entries included.

| Field | Present | Value |
| --- | --- | --- |
| `channel` | When the view has a channel | The channel name, without `#`. |
| `focus` | Agent targets only | The focal agent's handle. |
| `entries` | Always | The entries, described below. |
| `archived_count` | When prior-session lines are left out | How many lines `--all` would add. |

Each entry carries these fields. A field shown as optional is absent when empty or false.

| Field | Value |
| --- | --- |
| `from` | `user` for a prompt, `you` or `answered` for an answer, `@rimz` for a hidden delivery, otherwise the agent's handle. |
| `to` | Optional. The receiving agent's handle. Absent on agent output and flips. |
| `at` | The creation time when known, otherwise the recorded time, RFC 3339 in UTC. |
| `delivered_at` | Optional. The delivery record time when creation time is known, RFC 3339 in UTC. Included even for waits shorter than 60 seconds. |
| `text` | The body. For a flip, the note. |
| `message_id` | Optional. The id of the delivered message this entry records. |
| `reply_to` | Optional. Ids of the messages that opened the turn this entry belongs to. |
| `error` | Optional. `true` for a provider error. |
| `questions` | Optional. An ask's questions: `question`, `options` (a label string or `{label, description, caution}`), `multi_select`, `has_option_previews`. |
| `answers` | Optional. An answer's picks: `question`, `chosen`, `note`. |
| `stage` | Optional. A flip: `team`, `from`, `to`, `owner`, `by`. |

Entries carry no kind field. A prompt RimZ sent reads `from: "user"` like a human prompt, so JSON cannot tell the two apart. `reply_to` records causality as delivered. The human view builds threads from it, but only from turn output and replies back to the sender, so two entries linked in JSON can print in separate exchanges.

## See also

- [`rimz agents logs`](./agents.md#logs): one agent's transcript with the same scope, rendering, and hiding, plus `-f` to follow new lines.
- [Messaging guide](../../guide/messaging.md): sending messages and reading conversations back.
- [Internals: the transcript](../../internals/harness/transcript.md): entry kinds, causal links, and how threads are assembled.
