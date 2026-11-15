# Events CLI

`rimz events` reads and writes the workspace's durable event log. `follow` prints each agent lifecycle transition and each signal as one JSON object per line, so a script can react to agent state without reading panes. `emit` appends a named signal of your own and fires the loop tasks and workflow runs subscribed to it.

| Command | What it does |
| --- | --- |
| `rimz events list` | List recent signals from the active log. |
| `rimz events follow` | Stream lifecycle and signal lines as JSON Lines. |
| `rimz events emit <NAME>` | Append one signal and fire its subscribers. |

All take the [global flags](../cli.md#global-flags). They reach the room [the way room commands do](../cli.md#which-room-a-command-reaches), and `--root` picks the room whose log they use.

All three need an existing room, one with a readable workspace record. It does not have to be running. Outside a room they exit 1 with ``error: no room at <root>: run `rimz start` there, or pass --root <room>``. `follow` refuses before it polls. `emit` checks its name and payload first, so an invalid input refuses with its own error even outside a room.

| Exit code | When |
| --- | --- |
| `0` | `list` printed, including an empty result. `follow` ended by Ctrl-C, `SIGTERM`, or a closed pipe. `emit` appended its signal. |
| `1` | No room. A log `follow` cannot read. An `emit` [refusal](#emit-a-signal), or a firing failure after the append. |
| `2` | An argument error: a `--signal` value that is neither a valid name nor `FAMILY.*`, a `--since` value that does not parse, or a `-n` value that is not a non-negative integer. |

## List recent signals

```sh
rimz events list
rimz events list --signal 'ci.*' --since 2h -n 10
rimz events list --signal deploy.finished --json
```

`list` prints signals newest first, with columns `WHEN`, `SIGNAL`, `SOURCE`, and `SUBJECT`:

```console
$ rimz events list -n 6
WHEN  SIGNAL                SOURCE     SUBJECT
0s    deploy.finished ×3    cli        over 4s
4s    build.done            cli        -
4s    deploy.failed         cli        -
5s    deploy.finished       cli        -
53s   wait.wait-civil-yard  watch      -
1m    team.idle ×2          lifecycle  forge#probe · over 5s
```

The subject is the text that follows the signal name in a delivered notification, or `-` when empty. Text output folds adjacent signals with the same name, source, and subject into one row, and appends `×N` after the name. Payload fields outside the subject never split a row, so custom signals with an empty subject fold even when their payloads differ. When the folded members span at least one second, the row adds ` · over <span>` after the subject, or `over <span>` alone when the subject is empty. `WHEN` is the age of the newest member. Lifecycle records and the `agent.*` signals derived from them are not listed.

| Flag | Effect |
| --- | --- |
| `--signal <NAME\|FAMILY.*>` | Keep an exact name or every name in one family, using the [subscription selector grammar](./loop.md#signals). |
| `--since <DUR>` | Keep records within this duration of now. Units: `s`, `m`, `h`, `d` ([durations](../cli.md#durations)). |
| `-n, --limit <N>` | Maximum rows after filtering and folding in text, or records after filtering with `--json`. Default `20`. A folded row is never split. |
| `--json` | One [signal line](#signal-lines) per record, the same schema as `follow`, newest first. |

Only the active event-log generation is searched, never archives, even when `--since` reaches farther back. When no row is left, text output prints `no signal matched; active log reaches back <age>` (the age of the active log's first record) or `no signal matched; active log is empty`, and JSON output prints nothing.

## Follow the event stream

```sh
rimz events follow
rimz events follow --replay | jq 'select(.event == "signal")'
```

`follow` starts at the live edge and prints only what is appended after it starts. `--replay` first prints the current log generation from its beginning, then keeps following. The log rotates into an archive once it reaches 64 MiB, or at the smaller size a [`rimz workspace rotate-events --max-bytes`](./maintenance.md#rotate-the-event-log) run names. Archived generations are never replayed. A follower already running when the log rotates reads the rest of the old generation before it moves to the new one, so it misses nothing.

| Flag | Effect |
| --- | --- |
| `--replay` | Print the current log generation from its first record before following. |
| `--signal <NAME\|FAMILY.*>` | Print only matching signal lines, with no lifecycle lines. |
| `--json` | Accepted and ignored: JSON Lines is the only output. |

The stream follows these rules:

- stdout carries one JSON object per line, flushed as each line is written.
- stderr carries warnings, prefixed `rimz: warning:`, when an archived generation is missing or disappears before it is read, or the log generation moves backward. The stream continues after each warning.
- The follower checks the log every 250 ms. Set `RIMZ_EVENTS_POLL_MS` to a positive integer to change the interval. Any other value is ignored.

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
| `agent_name` | The agent's handle without `@`, when the record carries one. Omitted otherwise. |
| `parent_agent_id` | Session id of the root agent, on a subagent's line. Omitted for a root agent. |
| `signal` | The lifecycle signal the agent reported, tagged by its `signal` field, with that signal's own fields. The signals are listed in [the agent model](../../internals/agents/model.md#the-state-machine). |
| `prior_status` | Status before this transition. Omitted when the follower has no earlier state for the agent, including when the agent has fallen outside the 256 most recently ended agents. At the live edge that state comes from the workspace's current rollup. Under `--replay` it builds up from the replayed records, so an agent's first replayed line has none. |
| `status` | Status after the transition, as the lifecycle records it: one of the values in the [agent status table](./agents.md#list). An end that a reaper or a worktree removal writes over a `running` or `waiting` row reads `idle`, matching the card. |
| `phase` | Turn phase after the transition: `idle`, `reasoning`, `acting`, or `parked`. |
| `transition` | How the state machine classified the signal: `{"kind":"normal"}`, `{"kind":"reconciled","from":"…","reason":"…"}`, or `{"kind":"ignored","reason":"…"}`. |
| `compaction_closed` | `true` when this signal closed an open context compaction. |
| `waiting_cleared` | `true` when this signal moved the agent off `waiting`. |

A lifecycle record with no agent session id prints no line, and neither does a side conversation's record. A state snapshot (for example, an OpenCode conversation switch) also prints no lifecycle line, including under `--replay`. It still updates `prior_status` for the next real transition.

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
| `event_id` | Identity of the durable log record. `emit` prints the same id. |
| `at` | Record timestamp, RFC 3339. |
| `workspace_id` | Workspace whose log holds the record. |
| `name` | Signal name, such as `deploy.finished` or `ci.failed`. |
| `payload` | The signal's top-level JSON object, or `{}` when it has none. |
| `source` | Who appended it (see the next table). |

Every signal name, by who produces it. The families in the second column are [reserved](#reserved-families): only RimZ produces names in them.

| Names | Family | `source` | Produced by |
| --- | --- | --- | --- |
| Any valid name outside the reserved families | yours | `cli` | `rimz events emit` |
| `ci.passed`, `ci.failed` | `ci` | `forge` | The room's sidebar, when a watched checkout's CI changes state |
| `pr.opened`, `pr.merged`, `pr.closed`, `pr.behind`, `pr.conflicted`, `pr.queued`, `pr.dequeued` | `pr` | `forge` | The room's sidebar, when a watched checkout's pull request changes state |
| `trunk.moved` | `trunk` | `git` | The room's sidebar, when the project trunk ref moves |
| `worktree.created`, `worktree.removed` | `worktree` | `worktree` | A successful managed worktree creation or removal |
| `team.idle`, `team.waiting`, `team.failed`, `team.ended` | `team` | `lifecycle` | The agent lifecycle hook, when a team member's transition changes its cohort |
| `team.stage` ([payload](./teams.md#the-teamstage-signal)) | `team` | `team` | A team stage flip, and the [re-wake](./teams.md#the-owners-notice) of the stage owner when it is resumed |
| `wait.<task-name>` | `wait` | `watch` | The watcher of a `rimz wait` command, check, PID, or file watch, at a check-in and its final outcome |
| `agent.started`, `agent.idle`, `agent.waiting`, `agent.failed`, `agent.ended` | `agent` | none | Derived from a lifecycle record. No signal line: the lifecycle line is its trace. |

A `wait.<task-name>` delivery that RimZ sends because the watcher itself died also fires subscribers without a signal line. A watched command's exit code travels only in the delivered message, never in the payload. What each built-in signal's payload carries, and when it fires, is in [the loop reference](./loop.md#built-in-signals) and [the signal vocabulary](../../internals/harness/loops.md#the-signal-vocabulary).

## Emit a signal

```sh
rimz events emit deploy.finished
rimz events emit deploy.finished --json '{"env":"prod","version":"1.4.2"}'
```

`emit` appends one signal record with source `cli`, then fires every loop task subscribed to it in this workspace. It prints the record id, the count of tasks it started, and one indented line per task. Here `smoke` is a task added with `rimz loop add smoke --signal deploy.finished --repeat -- ...`:

```console
$ rimz events emit deploy.finished --json '{"env":"prod","version":"1.4.2"}'
emitted deploy.finished (evt_01a1dea841f674fb819852ff17c92c3a) · fired 1 task
  smoke
```

The part after `·` reads one of these:

| Receipt | When |
| --- | --- |
| `fired 1 task`, `fired N tasks` | That many tasks were handed the signal and spawned a detached run. Each is named on its own line. A busy task can still skip the trigger under its [overlap policy](./loop.md#overlapping-triggers). |
| `fired no task` | Tasks took the signal, and none started. |
| `no task is subscribed to it` | No task took the signal. Either no task subscribes to this name or its family, or every subscriber declined it: its `--match` rejected the payload, or it names a sibling in the family (a task on `deploy.failed` declines `deploy.finished` and records it as `skipped`). |

A task that took the signal but did not start gets an indented `<task>: did not start` line after any started task. A task does not start when it is disabled, paused, or a project task whose project is not trusted, or when its run fails to launch.

| Argument | Rule |
| --- | --- |
| `<NAME>` | Dot-separated segments, at most 64 bytes in total. Each segment starts with a lowercase letter or digit and contains only lowercase letters, digits, `-`, and `_`. The first segment is the signal's family. |
| `--json <OBJECT>` | One top-level JSON object, at most 64 KiB as typed (whitespace counts). Omitted, the payload is `{}`. |

Firing happens inside the `emit` process, with no daemon. It needs an existing room but no running agent. It spawns one detached run for each live subscription that exists in this workspace at that moment, so a task added afterwards never sees the signal. A busy task can keep its trigger through its [overlap policy](./loop.md#overlapping-triggers). When the room has an active [workflow](./workflow.md) run, `emit` also starts one detached workflow advance, which hands the signal to the run's subscribed checks and steps. The receipt does not mention the advance. What a subscription matches, what it delivers, and when a delivery reaches its agent are in [`rimz loop` signals](./loop.md#signals).

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

Seven families belong to RimZ: `agent`, `team`, `ci`, `pr`, `trunk`, `worktree`, and `wait`. `emit` refuses every name in them, including names RimZ never produces (`ci.custom`):

```console
$ rimz events emit ci.passed
error: signal name `ci.passed` is reserved for RimZ
```

The sidebar appends forge signals by running `emit` with a hidden `--source forge` flag, which accepts only the nine `ci` and `pr` names. The hidden `--source git` accepts only `trunk.moved`. There is no CLI `worktree` source. These flags perform no caller check, so reserved families prevent name collisions, not authenticated provenance.

The producers follow these rules beyond the [built-in signal table](./loop.md#built-in-signals):

- `pr.opened` reports a newly observed open PR after a successful prior probe, not a PR already present when monitoring starts.
- `trunk.moved` reports a changed commit on the same trunk ref.
- Worktree signals follow successful managed operations, with creation after its hook succeeds. Reuse and failed-creation rollback are silent. [Trunk and worktree signals](../../internals/harness/loops.md#trunk-and-worktree-signals) has the producer contract.
- `pr.queued` fires when the PR enters the merge queue, and again only for a later entry. `pr.dequeued` fires when it leaves the queue while still open, again for a later removal, and when a re-arm is withdrawn, with the original `dequeued_at` in that last case. A PR that merges out of the queue emits `pr.merged` and no `pr.dequeued`. The `queued_at` and `dequeued_at` payload timestamps are GitHub's.
- The room's first poll of a checkout only records the queue state as a baseline and emits nothing. A room that opens on a PR already queued or dequeued therefore hears nothing: read that standing state with [`--when pr.queue=dequeued`](./loop.md#conditions) instead. A checkout the room already watched before the PR entered the queue emits on the first poll that reads the change, and a removal that GitHub had already re-armed by that poll emits no `pr.dequeued`.
- `team.waiting` follows the same hook-time transition as `agent.waiting`, so a Codex permission request under automatic review fires it at once ([Codex asks](../../internals/agents/adapter_codex.md#asks-and-answers)).
