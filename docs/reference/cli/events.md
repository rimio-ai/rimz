# `rimz events`

`rimz events` is the event seam in both directions. `follow` streams each durable agent lifecycle transition and each emitted signal as one JSON object per line, for harnesses that react to agent state without scraping panes or sidebar output. `emit` puts a named signal into the same durable log and fires the tasks subscribed to it.

## Follow lifecycle transitions

```sh
rimz events follow
rimz events follow --replay
rimz --root /path/to/project events follow --json
```

`follow` starts at the current live edge and waits for new lifecycle events. `--replay` first reads the current active event-log generation from its beginning, then follows new events. Archived generations remain outside replay scope; a follower that is already running drains a rotation's archived tail before it continues on the new active log.

JSON Lines is the command's only output format. `--json` is accepted for consistency with other scripting commands and is implied. Each line is flushed as it is emitted. Logs and archive-gap warnings use stderr, and a downstream reader closing the pipe ends the command successfully.

The read-only follower lives in [`store::follow`](../../../crates/rimz/src/store/follow.rs) and polls the durable log every 250 ms by default. Set `RIMZ_EVENTS_POLL_MS` to a positive integer number of milliseconds to tune that interval.

## Lifecycle event schema

Every lifecycle line has this shape:

```json
{"event":"lifecycle","v":1,"event_id":"evt_…","at":"2026-06-01T12:00:00Z","workspace_id":"ws_…","kind":"claude","agent_id":"session-1","agent_name":"coder","signal":{"signal":"turn_ended","errored":false,"parked_on_background":false},"prior_status":"running","status":"success","phase":"idle","transition":{"kind":"normal"},"compaction_closed":false,"waiting_cleared":false}
```

| Field | Meaning |
| --- | --- |
| `event` | Line class: `lifecycle` here, `signal` for an emitted signal. |
| `v` | Lifecycle envelope schema version. A breaking wire change increments this value. |
| `event_id` | Durable event-log record identity. |
| `at` | Event timestamp in RFC 3339 form. |
| `workspace_id` | Workspace whose event log owns the record. |
| `kind` | Agent integration kind, such as `claude` or `codex`. |
| `agent_id` | Provider session identity for the root agent or subagent. |
| `agent_name` | RimZ card name when the observation carries one; omitted otherwise. |
| `parent_agent_id` | Root session identity for a subagent event; omitted for root agents. |
| `signal` | The complete [`LifecycleSignal`](../../internals/agents/model.md#the-state-machine) object, including variant-specific fields. |
| `prior_status` | Raw lifecycle status immediately before this signal; omitted when no prior state exists in the followed generation. |
| `status` | Raw lifecycle status after the transition. |
| `phase` | Turn phase after the transition: `idle`, `reasoning`, `acting`, or `parked`. |
| `transition` | Classification object: `{"kind":"normal"}`, `{"kind":"reconciled","from":"…","reason":"…"}`, or `{"kind":"ignored","reason":"…"}`. |
| `compaction_closed` | Whether this signal closed an open compaction bracket. |
| `waiting_cleared` | Whether this signal durably moved the agent off `waiting`. |

A malformed lifecycle record without an agent identity remains in the audit log but cannot form this strongly typed envelope, so the follower skips it just as the lifecycle rollup quarantines it. Records that are neither `agent.lifecycle` nor `signal.emit` are not projected at all.

## Emit a signal

```sh
rimz events emit deploy.finished
rimz events emit ci.finished --json '{"conclusion":"failure","branch":"main"}'
```

`emit` appends one durable signal record, then fires every wake and loop task in this workspace whose `--signal` subscription matches, in the emitting process:

```console
$ rimz events emit ci.finished --json '{"conclusion":"failure","branch":"main"}'
emitted ci.finished (evt_01a06d7d112171d0bdaceff9e4a3c6aa) · fired 1 tasks
  wake-noble-lane
```

A name is lowercase dot-separated words, at most 64 bytes, each segment starting with a lowercase letter or digit and otherwise using letters, digits, `-`, or `_`. The `agent.` and `wake.` prefixes are reserved for RimZ's own lifecycle and watched-command signals, so `emit` refuses them. `--json` takes one top-level JSON object of at most 64 KiB; subscribers filter on its top-level fields with `--match KEY=VALUE`, and a task's prompt can interpolate them as `{{key}}`.

Firing has no daemon behind it and no queue in front of it. The emitting process resolves the subscribers itself and spawns one detached run per match, so a signal reaches only the tasks armed for this workspace at that instant: a task armed a second later does not see it, and nothing is replayed when a room opens. A wake armed on a signal fires without a room open, unlike a `--in` delay, which waits for the room's elder or the loop timer.

Emitted signals rejoin the stream `follow` prints:

```console
$ rimz events follow --replay
{"event":"signal","v":1,"event_id":"evt_01a06d7d112171d0bdaceff9e4a3c6aa","at":"2026-09-04T17:35:08.065761436Z","workspace_id":"ws_f89e49906df0621ad2765112","name":"ci.finished","payload":{"branch":"main","conclusion":"failure"},"source":"cli"}
```

`source` is `cli` for `rimz events emit`, `forge` for a pull-request or CI transition the room's sidebar observed, and `watch` for a `rimz wake -- <command>` completion. Agent lifecycle transitions also fire `agent.*` signals to subscribers, but they carry no separate `signal` line, because the `lifecycle` line is already their durable record. What each built-in signal carries is in [loops.md → the signal vocabulary](../../internals/harness/loops.md#the-signal-vocabulary), and arming a one-shot subscription is [`rimz wake --signal`](./wake.md#triggers).
