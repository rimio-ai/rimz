# `rimz events`

`rimz events follow` streams each durable agent lifecycle transition as one JSON object per line, for harnesses that react to agent state without scraping panes or sidebar output.

## Follow lifecycle transitions

```sh
rimz events follow
rimz events follow --replay
rimz --root /path/to/project events follow --json
```

`follow` starts at the current live edge and waits for new lifecycle events. `--replay` first reads the current active event-log generation from its beginning, then follows new events. Archived generations remain outside replay scope; a follower that is already running drains a rotation's archived tail before it continues on the new active log.

JSON Lines is the command's only output format. `--json` is accepted for consistency with other scripting commands and is implied. Each line is flushed as it is emitted. Logs and archive-gap warnings use stderr, and a downstream reader closing the pipe ends the command successfully.

The follower polls the durable log every 250 ms by default. Set `RIMZ_EVENTS_POLL_MS` to a positive integer number of milliseconds to tune that interval.

## Lifecycle event schema

Every line has this shape:

```json
{"v":1,"event_id":"evt_…","at":"2026-06-01T12:00:00Z","workspace_id":"ws_…","kind":"claude","agent_id":"session-1","agent_name":"coder","signal":{"signal":"turn_ended","errored":false,"parked_on_background":false},"prior_status":"running","status":"success","phase":"idle","transition":{"kind":"normal"},"compaction_closed":false,"waiting_cleared":false}
```

| Field | Meaning |
| --- | --- |
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

The stream projects `agent.lifecycle` records only. A malformed lifecycle record without an agent identity remains in the audit log but cannot form this strongly typed envelope, so the follower skips it just as the lifecycle rollup quarantines it.
