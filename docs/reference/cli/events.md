# `rimz events`

`rimz events` reads and writes the workspace's durable event log. `follow` prints each agent lifecycle transition and each signal as one JSON object per line, so a script can react to agent state without reading panes. `emit` appends a named signal of your own and fires the loop tasks subscribed to it.

| Command | What it does |
| --- | --- |
| `rimz events follow` | Stream lifecycle and signal lines as JSON Lines. |
| `rimz events emit <NAME>` | Append one signal and fire its subscribers. |

Both take the [global flags](../cli.md#global-flags); `--root` picks the workspace whose log they use.

## Follow the event stream

```sh
rimz events follow
rimz events follow --replay | jq 'select(.event == "signal")'
```

`follow` starts at the live edge and prints only what is appended after it starts. `--replay` first prints the current log generation from its beginning, then keeps following. The log rotates into an archive once it reaches 64 MiB, or when you run [`rimz workspace rotate-events`](./maintenance.md#rotate-the-event-log), and archived generations are never replayed. A follower that is already running when the log rotates reads the rest of the old generation before it moves to the new one, so it misses nothing.

| Flag | Effect |
| --- | --- |
| `--replay` | Print the current log generation from its first record before following. |
| `--json` | Accepted and ignored: JSON Lines is the only output. |

The stream follows these rules:

- stdout carries one JSON object per line, flushed as each line is written.
- stderr carries warnings, prefixed `rimz: warning:`, when an archived generation is missing or disappears before it is read, or the log generation moves backward. The stream continues after each warning.
- Ctrl-C, `SIGTERM`, or a reader closing the pipe ends the command with exit code 0. A log that cannot be read ends it with exit code 1.
- The follower checks the log every 250 ms. Set `RIMZ_EVENTS_POLL_MS` to a positive integer to change the interval; any other value is ignored and the default applies.

Each line's `event` field says which shape it has: `lifecycle` or `signal`. The log's other records (messages, launches, attaches) are not printed. How the follower folds the log into lines is in [publishing transitions](../../internals/agents/model.md#publishing-transitions).

## Lifecycle lines

A lifecycle line is one transition of one agent or provider-native subagent:

```json
{"event":"lifecycle","v":1,"event_id":"evt_…","at":"2026-06-01T12:00:00Z","workspace_id":"ws_…","kind":"claude","agent_id":"session-1","agent_name":"coder","signal":{"signal":"turn_ended","errored":false,"parked_on_background":false},"prior_status":"running","status":"success","phase":"idle","transition":{"kind":"normal"},"compaction_closed":false,"waiting_cleared":false}
```

| Field | Meaning |
| --- | --- |
| `event` | `lifecycle`. |
| `v` | Schema version of the lifecycle line, currently `1`. A breaking change to the line increments it. |
| `event_id` | Identity of the durable log record. |
| `at` | Record timestamp, RFC 3339. |
| `workspace_id` | Workspace whose log holds the record. |
| `kind` | Agent kind, such as `claude` or `codex`. |
| `agent_id` | Provider session id of the agent or subagent. |
| `agent_name` | The agent's handle without `@`, when the record carries one; omitted otherwise. |
| `parent_agent_id` | Session id of the root agent, on a subagent's line; omitted for a root agent. |
| `signal` | The lifecycle signal the agent reported, tagged by its `signal` field, with that signal's own fields. The signals are listed in [the agent model](../../internals/agents/model.md#the-state-machine). |
| `prior_status` | Status before this transition; omitted when the follower has no earlier state for the agent. At the live edge that state comes from the workspace's current rollup; under `--replay` it builds up from the replayed records, so an agent's first replayed line has none. |
| `status` | Status after the transition, as the lifecycle records it (for example `running`, `waiting`, `success`). |
| `phase` | Turn phase after the transition: `idle`, `reasoning`, `acting`, or `parked`. |
| `transition` | How the state machine classified the signal: `{"kind":"normal"}`, `{"kind":"reconciled","from":"…","reason":"…"}`, or `{"kind":"ignored","reason":"…"}`. |
| `compaction_closed` | `true` when this signal closed an open context compaction. |
| `waiting_cleared` | `true` when this signal moved the agent off `waiting`. |

A lifecycle record with no agent session id prints no line.

## Signal lines

A signal line is one signal appended to the log, by `emit` or by RimZ itself:

```console
$ rimz events follow --replay
{"event":"signal","v":1,"event_id":"evt_01a06d7d112171d0bdaceff9e4a3c6aa","at":"2026-09-04T17:35:08.065761436Z","workspace_id":"ws_f89e49906df0621ad2765112","name":"deploy.finished","payload":{"env":"prod","version":"1.4.2"},"source":"cli"}
```

| Field | Meaning |
| --- | --- |
| `event` | `signal`. |
| `v` | Schema version of the signal line, currently `1`. |
| `event_id` | Identity of the durable log record; `emit` prints the same id. |
| `at` | Record timestamp, RFC 3339. |
| `workspace_id` | Workspace whose log holds the record. |
| `name` | Signal name, such as `deploy.finished` or `ci.failed`. |
| `payload` | The signal's top-level JSON object; `{}` when it has none. |
| `source` | Who appended it; see the next table. |

| `source` | Appended by | Names |
| --- | --- | --- |
| `cli` | `rimz events emit` | Any valid name outside the reserved families. |
| `forge` | The room's sidebar, when a worktree branch's CI or pull request changes state | `ci.passed`, `ci.failed`, `pr.merged`, `pr.closed` |
| `lifecycle` | The agent lifecycle hook, when a team member's transition changes its cohort | `team.idle`, `team.waiting`, `team.failed`, `team.ended` |
| `team` | A team stage flip, and the re-wake of the stage owner when it registers | `team.stage` |
| `watch` | The watcher of a `rimz wait -- <command>`, at a check-in and when the command exits | `wait.<task-name>` |

Two kinds of signal fire subscribers but print no signal line. An `agent.*` signal is derived from a lifecycle record, and that record's lifecycle line is its durable trace. A `wait.<task-name>` delivery that RimZ sends because the watcher itself died is not written to the log. A watched command's exit code travels only in the delivered message, never in the signal line's payload.

## Emit a signal

```sh
rimz events emit deploy.finished
rimz events emit deploy.finished --json '{"env":"prod","version":"1.4.2"}'
```

`emit` appends one signal record with source `cli`, then fires every loop task subscribed to it in this workspace, and prints the record id, the count of tasks it fired, and one indented line per task:

```console
$ rimz events emit deploy.finished --json '{"env":"prod","version":"1.4.2"}'
emitted deploy.finished (evt_01a06d7d112171d0bdaceff9e4a3c6aa) · fired 1 tasks
  wait-noble-lane
```

With no subscriber the line reads `fired 0 tasks` and nothing follows. The count leaves out a task that observed the signal and recorded `skipped`, such as a subscription to `deploy.failed`.

| Argument | Rule |
| --- | --- |
| `<NAME>` | Dot-separated segments, at most 64 bytes in total. Each segment starts with a lowercase letter or digit and contains only lowercase letters, digits, `-`, and `_`. The first segment is the signal's family. |
| `--json <OBJECT>` | One top-level JSON object, at most 64 KiB as typed (whitespace counts). Omitted, the payload is `{}`. |

Firing happens inside the `emit` process, with no daemon and no queue, so `emit` needs no open room. It spawns one detached run for each enabled subscription that exists in this workspace at that moment; a task added afterwards never sees the signal, and nothing replays it later. A disabled or paused task, or a project task whose project is untrusted, does not fire. What a subscription matches, what it delivers, and when a delivery reaches its agent are in [`rimz loop` signals](./loop.md#signals).

`emit` refuses these inputs with exit code 1, before it writes anything:

| Input | Error |
| --- | --- |
| A name that breaks the rules | ``error: invalid signal name `<name>`; use lowercase dot-separated words, at most 64 bytes`` |
| A name in a reserved family | ``error: signal name `<name>` is reserved for RimZ`` |
| A payload over 64 KiB | `error: --json payload exceeds 64 KiB` |
| A payload that is not valid JSON | `error: parsing --json payload`, followed by the parser's message |
| A payload that is JSON but not an object | `error: --json payload must be a JSON object` |

If firing fails after the append, `emit` exits 1 with `firing signal tasks`, and the signal record stays in the log.

### Reserved families

Five families belong to RimZ, and `emit` refuses every name in them, including names RimZ never produces (`ci.custom`):

```console
$ rimz events emit ci.passed
error: signal name `ci.passed` is reserved for RimZ
```

| Family | Names RimZ produces | Source |
| --- | --- | --- |
| `agent` | `agent.started`, `agent.idle`, `agent.waiting`, `agent.failed`, `agent.ended` | Derived from lifecycle records; no signal line. |
| `team` | `team.idle`, `team.waiting`, `team.failed`, `team.ended` | `lifecycle` |
| `team` | `team.stage` ([payload](./teams.md#the-teamstage-signal)) | `team` |
| `ci` | `ci.passed`, `ci.failed` | `forge` |
| `pr` | `pr.merged`, `pr.closed` | `forge` |
| `wait` | `wait.<task-name>` | `watch` |

The sidebar appends forge signals by running `emit` with a hidden `--source forge` flag, which accepts only the four `ci` and `pr` names above. The flag performs no caller check, so the reserved families keep scripts from colliding with RimZ's names but do not authenticate a forge signal. What each built-in signal's payload carries is in [the loops guide](../../guide/loops.md#signals-the-rooms-event-bus) and [the signal vocabulary](../../internals/harness/loops.md#the-signal-vocabulary).
