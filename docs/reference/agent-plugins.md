# Agent plugins

An agent plugin connects an agent CLI that RimZ has no built-in adapter for. It is a bundle directory with three parts: a manifest that declares the agent's identity, launch flags, and what it reports; a shim the agent's own hooks call, which translates native events into RimZ's canonical JSON envelope; and optional probe executables RimZ runs to read spend, account, and version.

> **Early surface, not ready for public use.** The bundle format, the canonical envelope, and the probe contracts are under active development and change without notice. Prototype against them and send feedback, but do not depend on them in production.

A plugin is machine configuration: RimZ runs the launch, resume, and probe commands its manifest declares. Install only bundles you trust. Project configuration cannot register a plugin, so plugin commands sit outside [project trust](./cli/hooks-trust.md#project-trust).

## Register a bundle

`rimz agents register <kind>` creates a working skeleton under `~/.rimz/agents.d/<kind>/` (`$RIMZ_HOME/agents.d/<kind>/` when `RIMZ_HOME` is set):

```console
$ rimz agents register mybot
registered `mybot` at /home/me/.rimz/agents.d/mybot
edit agent.toml and README.md, then run `rimz agents register --check`
```

| File | Starts as |
| --- | --- |
| `agent.toml` | A valid manifest that emits only `session_start`, with the launch flags, `[transcripts]`, and `[probes]` commented out. |
| `README.md` | The setup document `rimz doctor` points at. Write the agent's hook installation steps here. |
| `shim.sh` | The forwarding shim from [Shim contract](#shim-contract), executable. |
| `probes/spend` | A stub spend probe that returns no entries, executable. |
| `probes/account` | A stub account probe that reports logged out, executable. |

The scaffold's `[probes]` entries are commented out, so the stub probes do nothing until you uncomment them; until then `rimz agents check` prints `probes: none declared`.

`register` refuses a kind that breaks the [kind rules](#top-level-keys), a built-in kind such as `claude`, a kind a valid bundle already uses, and a directory that already exists.

`rimz agents register --check` validates every bundle under `agents.d` and creates nothing. It prints `N agent plugin(s) valid`, or exits 1 listing each invalid manifest with its path and error.

The complete [ScriptBot example](../../examples/agent-plugin/README.md) adds a fake agent, a shim, a priced spend probe, an account probe, and a fixture transcript.

## Check a plugin

`rimz agents check <kind>` is the authoring loop: run it after each change to the manifest, shim, or probes.

```sh
rimz agents check mybot
rimz agents check mybot --spend-file ~/.mybot/sessions/sess-123.jsonl
rimz agents check mybot --replay ./events.jsonl
```

| Flag | Effect |
| --- | --- |
| `--spend-file <PATH>` | Runs the spend probe on this transcript. Without it, the spend probe is reported `skipped`. |
| `--replay <JSONL>` | Parses each line as one envelope and steps it through RimZ's agent state machine, without touching any room. |

The report names the manifest, counts the [coverage](#coverage-and-doctor) the manifest derives, and runs each declared probe. The account and version probes always run. This run used the manifest from [Manifest](#manifest); the stderr warning lines for undeclared events are left out:

```console
$ rimz agents check mybot --spend-file sess.jsonl --replay events.jsonl
plugin `mybot`
manifest: valid (/home/me/.rimz/agents.d/mybot/agent.toml)
coverage: 5 wired, 1 partial, 12 unsupported
lifecycle: 4 native, 1 derived, 6 absent
probes:
  spend: ok (present, executable; ./probes/spend) — 0 entries
  account: ok (present, executable; ./probes/account) — canonical account response
  version: ok (present, executable; mybot --version) — mybot 1.4.2
replay: events.jsonl
LINE  EVENT           SIGNAL          STATE              RESULT
1     session_start   registered      idle               ok
2     turn_start      turn_started    running/reasoning  ok
3     tool_use        tool_used       running/acting     ok
4     awaiting_input  awaiting_input  waiting            warning: event `awaiting_input` is absent from manifest emits
5     turn_end        rejected        unchanged          error: invalid canonical envelope: invalid type: string "yes", expected a boolean
6     turn_end        turn_ended      success            ok
7     future_event    unknown         unchanged          warning: event `future_event` is absent from manifest emits
8     turn_start      rejected        unchanged          error: lifecycle observation has no agent identity
final AgentState:
  sess-123: status=success, phase=idle, compacting=false
error: agent plugin check failed
```

Each probe line is `ok`, `skipped`, or `failed`, followed by whether the executable exists and has its executable bit, the command, and a detail: the entry count, the version string, or the failure reason.

A replay row's `SIGNAL` is the lifecycle signal the envelope produced, `rejected` when RimZ would drop it, or `unknown` for an event name outside [the vocabulary](#events). `STATE` is the agent's state after the row. A row is rejected for invalid JSON, a missing `hook_event_name`, a field of the wrong type, a `protocol` other than 1, a missing `session_id`, or an `agent_id` that fails [subagent identity](#subagent-identity). An event missing from `emits` is accepted with a warning, as live ingestion accepts it.

Replay differs from a live room in two places. A live room drops the rejected envelopes without an error. And a live room records no waiting state for `awaiting_input` when the manifest sets `native-ask-ui = false`, while replay still shows `waiting` ([capabilities](#capabilities)).

`check` exits 1 when any declared probe is missing, is not executable, or fails, when any replay row is rejected, or when the `--spend-file` or `--replay` file cannot be read. `check --spend-file` also fails a spend entry whose timestamp is not RFC 3339, which a live room skips. `check` refuses a built-in kind, a kind with no bundle, and an invalid manifest, printing the manifest error.

## Bundle layout

```text
~/.rimz/agents.d/mybot/
├── agent.toml        manifest
├── README.md         setup-doc: how to install the agent's hooks
├── shim.sh           called by the agent's hooks
└── probes/
    ├── spend
    └── account
```

RimZ reads every `agents.d/*/agent.toml` once per process. The directory name must equal the manifest's `kind`.

These plugin manifests remain TOML. They register provider kinds and are separate from the [Markdown definitions](./definitions.md) that select models, tools, crafts, and team seats.

RimZ installs no hooks for a plugin: `rimz hooks install` has no installer for an unknown agent, so wiring the agent's native hooks to the shim is the bundle's job, documented in its `setup-doc`.

## Manifest

A manifest using every field that has an effect:

```toml
protocol = 1
kind = "mybot"
display-name = "MyBot"
process-names = ["mybot", "node"]
emits = ["session_start", "turn_start", "turn_end", "tool_use", "context"]
setup-doc = "README.md"

[brand]
emblem = "[mb]"
color = 141
color-rgb = [175, 135, 255]

[capabilities]
native-ask-ui = true
subagents = false
registers-lazily = false
context-usage = true

[tools]
mutating = ["write", "shell"]
editing = ["write"]

[launch]
bin = "mybot"
args = []
model-flag = "--model"
effort-flag = "--effort"
system-prompt-file-flag = "--system-prompt-file"
resume = ["mybot", "--resume", "{session_id}"]
compact-command = "/compact"

[launch.permission-args]
ask = []
auto = ["--auto"]
yolo = ["--yolo"]
plan = ["--plan"]

[transcripts]
globs = ["~/.mybot/sessions/*.jsonl"]
thread-key = "per-file"

[probes]
spend = ["./probes/spend"]
account = ["./probes/account"]
version = ["mybot", "--version"]
```

Keys are kebab-case. RimZ ignores keys it does not recognise in every table, so a misspelled key such as `native_ask_ui` has no effect and raises no error; `rimz agents check` shows the coverage the manifest actually derives.

### Top-level keys

| Key | Required | Meaning |
| --- | --- | --- |
| `protocol` | yes | Must be `1`. |
| `kind` | yes | The agent's name in `rimz agents <kind>`, profiles, and addresses. Lowercase letters, digits, and `-`, starting and ending with a letter or digit. Must equal the bundle directory name and differ from every built-in kind and every other plugin. |
| `display-name` | yes | Name shown in the sidebar and reports. Must not be empty. |
| `process-names` | yes | Process names RimZ matches to find the agent running in a pane. At least one, no duplicates. |
| `emits` | yes | The canonical events the shim sends, from [Events](#events). Must include `session_start`, which creates the session. No duplicates or unknown names. |
| `setup-doc` | yes | Path to the hook installation guide, relative to the bundle. The file must exist. |

### `[brand]`

| Key | Default | Meaning |
| --- | --- | --- |
| `emblem` | the shared fallback emblem | Short text shown as the agent's emblem. |
| `color` | `141` | Terminal 256-color index. |
| `color-rgb` | `[175, 135, 255]` | Truecolor value. |

### `[capabilities]`

Every key defaults to `false`.

| Key | Meaning |
| --- | --- |
| `native-ask-ui` | The agent shows its own permission, plan, and question prompts. RimZ records `awaiting_input` as a waiting agent only when this is `true`; when `false`, those events change nothing. |
| `subagents` | The agent runs child agents. The subagent coverage claim needs this plus `subagent_start` and `subagent_end` in `emits`. |
| `registers-lazily` | The agent can be running before its first `session_start` arrives, so RimZ binds a late session to its pane by working directory. |
| `context-usage` | Claims a context gauge in coverage. Emitting `context` claims it too. |
| `background-tasks` | Accepted and has no effect. |

### `[tools]`

| Key | Meaning |
| --- | --- |
| `mutating` | `tool_name` values that change files or state. A `tool_use` naming one, with `is_error` false, counts as a mutation. |
| `editing` | `tool_name` values that edit files. Each must also appear in `mutating`. |

### `[launch]`

Without `[launch]`, RimZ has no command to start the agent. With it, `rimz agents mybot "fix the parser"` runs `bin`, then `args`, then the mode, model, effort, and prompt-file flags and any profile `args`, then `--` and the prompt.

| Key | Meaning |
| --- | --- |
| `bin` | Executable. Must not be empty. |
| `args` | Arguments always passed after `bin`. |
| `model-flag` | Flag that takes the `model` value, as in `--model mybot-pro`. |
| `effort-flag` | Flag that takes the `effort` value. |
| `system-prompt-file-flag` | Flag that takes the path of the composed system prompt file and replaces the agent's system prompt. Declare it only for a flag that reads a file and replaces the whole prompt. |
| `resume` | Full argv to resume a session. Must contain `{session_id}`, which RimZ replaces with the session id. |
| `compact-command` | Text RimZ sends into the pane to compact the agent (`rimz agents compact` and smart compaction). Must not be empty. RimZ sends it bare: `rimz agents compact` refuses an instruction for a plugin, and refuses the plugin entirely unless `emits` includes `turn_start`. |

A launch that sets `model`, `effort`, or `system-prompt-file` fails before any pane opens when the matching flag is not declared. A launch that sets `auto-compact` always fails for a plugin, since the manifest has no field for it ([auto-compaction window](./agent-support.md#auto-compaction-window)).

`[launch.permission-args]` holds the arguments each permission mode (`ask`, `auto`, `yolo`, `plan`) appends. Each defaults to no arguments, so the agent keeps its own default. Modes are described in [permission modes](./agent-support.md#permission-modes).

### `[transcripts]`

| Key | Default | Meaning |
| --- | --- | --- |
| `globs` | none | Transcript file patterns. `~/` expands to `$HOME`, an absolute pattern stands as written, and a relative pattern resolves from the bundle directory. |
| `thread-key` | `per-file` | `per-file` counts each transcript file as one session; `session-dir` counts each directory as one. |

Spend history needs both `[transcripts]` and a spend probe: RimZ sends each matching file to the probe.

### `[probes]`

Each probe is an argv array: `spend`, `account`, and `version`. See [Probe contracts](#probe-contracts).

### Paths

An executable in `bin`, `resume`, or a probe resolves the same way. An absolute path is used as written, a path containing `/` (such as `./probes/spend`) resolves from the bundle directory, and a bare name (such as `mybot`) is looked up on `PATH`.

## Canonical event envelope

The shim sends one JSON object per event. Three fields are required:

| Field | Meaning |
| --- | --- |
| `protocol` | `1`. Any other value is dropped. |
| `hook_event_name` | One of the [events](#events). |
| `session_id` | The root session's id. An event without one updates no session. |

### Subagent identity

A child agent's events carry the parent's id in `session_id` and the child's own id in `agent_id`. A `subagent_start` or `subagent_end` whose `agent_id` is missing, blank, or equal to `session_id` is dropped. Any other event whose `agent_id` differs from its `session_id` is treated as the child's and dropped, so it never advances the parent.

### Shared fields

Any event may carry these optional fields:

| Field | Type | Meaning |
| --- | --- | --- |
| `agent_id` | string | Child id, see [Subagent identity](#subagent-identity). |
| `cwd` | string | Working directory; RimZ uses it to match the agent to a worktree. |
| `model` | string | Model in use. |
| `effort` | string | Effort level in use. |
| `context_pct` | integer | Percent of the context window used; values above 100 read as 100. |
| `context_window` | integer | Context window size in tokens. |
| `total_tokens` | integer | Tokens in context. |
| `input_tokens`, `output_tokens` | integer | Token counts for the latest turn. |
| `cache_read_input_tokens`, `cache_write_input_tokens` | integer | Cache token counts. |
| `transcript_path` | string | The session's transcript file. |
| `total_cost_usd` | number | Session cost in US dollars. Read from `context` events only. |
| `rate_limits` | object | `{"windows": [...]}` in the [window shape](#account). Read from `context` events only. |

### Events

| Event | Event fields | Effect |
| --- | --- | --- |
| `session_start` | none | Registers the session. |
| `turn_start` | `prompt?` | Starts a turn; `prompt` becomes the turn's task. |
| `turn_end` | `errored?`, `error_message?`, `last_assistant_message?` | Ends the turn, as failed when `errored` is `true`. `last_assistant_message` becomes a supervised run's output. |
| `tool_use` | `tool_name?`, `is_error?` | Records tool activity, classified by [`[tools]`](#tools). |
| `awaiting_input` | `ask` (`permission`, `plan_approval`, or `question`), `question?` | Marks the agent waiting, when `native-ask-ui = true`. |
| `compaction_start` | none | Marks the agent compacting. |
| `compaction_end` | `trigger?` (`auto` or `manual`) | Ends compaction. |
| `subagent_start` | child `agent_id` | Registers a child under the parent `session_id`. |
| `subagent_end` | child `agent_id`, `errored?` | Ends the child as succeeded or failed. |
| `session_end` | none | Ends the session and hides its card; the session stays available to resume. |
| `context` | shared fields, plus the context fields below | Updates the card's context, cost, and rate limits without changing state. |

A `context` event also accepts the optional fields of RimZ's [`AgentContext`](../../crates/rimz/src/agents/context.rs) record, including `session_name`, `session_preview`, `model_id`, `effort`, `model_display_name`, `thinking_enabled`, `output_style`, `vim_mode`, `agent_version`, `cost`, `tokens`, `rate_limits`, `pr`, and `account`. The top-level `model`, token, `context_pct`, `total_cost_usd`, and `rate_limits` fields fill the same record when its own fields are absent. RimZ sets `source` and `observed_at` itself.

### Dropped envelopes

RimZ drops an envelope without an error, changing nothing, when a field has the wrong type (such as `"errored": "yes"`), `awaiting_input` has no valid `ask`, `protocol` is not 1, or `hook_event_name` differs from the shim's `--event`. An event name outside the vocabulary is also ignored. An event that is valid but missing from `emits` is processed, and RimZ logs a warning once per event; add it to `emits` so coverage stays accurate. `rimz agents check --replay` shows each of these decisions.

## Shim contract

The agent's hook calls the shim with the event name as its argument and the envelope on stdin. The scaffold's shim forwards an envelope that is already canonical:

```sh
#!/bin/sh
set -eu
event=${1:?canonical event name}
exec rimz hooks feed --source mybot --event "$event"
```

`--source` is the plugin's `kind`. `--event` is optional; without it, RimZ reads the event from `hook_event_name`, and when both are given they must match.

`rimz hooks feed` prints nothing on stdout for a plugin event, and a dropped envelope still exits 0. Stdin that is not JSON exits 1. RimZ never answers a prompt on the agent's behalf: the user answers in the agent's own UI. Before setting `native-ask-ui = true`, confirm that an empty, successful hook response leaves the agent's prompt in place.

## Probe contracts

Probes are optional enrichment. RimZ runs each one in the bundle directory, with stdin, stdout, and stderr piped:

| Limit | Value |
| --- | --- |
| Time | Killed after 3 seconds. |
| stdout | At most 1 MiB. |
| stderr | The first 16 KiB, quoted in the warning when the probe exits nonzero. |
| Failure | A missing executable, nonzero exit, timeout, oversized output, or invalid JSON drops that reading and logs a warning. Session state is never affected. |

### Spend

RimZ sends one JSON line per transcript file matched by [`[transcripts]`](#transcripts):

```json
{"file":"/home/me/.mybot/sessions/sess-123.jsonl","cursor":{"line":18}}
```

`cursor` is `null` on the first request for a file, and afterwards whatever the probe last returned. Return the entries after that cursor and a new cursor:

```json
{
  "entries": [
    {
      "thread_id": "sess-123",
      "timestamp": "2026-06-01T12:00:00Z",
      "model": "mybot-pro",
      "input_tokens": 1200,
      "output_tokens": 300,
      "cache_read": 400,
      "cache_write": 100,
      "cost_usd": 0.02
    }
  ],
  "cursor": {"line":19},
  "origin": "/work/project"
}
```

| Field | Meaning |
| --- | --- |
| `entries[].timestamp` | Required, RFC 3339. An entry with an unparseable timestamp is skipped. |
| `entries[].cost_usd` | Cost in US dollars, used as given; RimZ applies no price table. Missing counts as 0. |
| `entries[].thread_id`, `model`, `input_tokens`, `output_tokens`, `cache_read`, `cache_write` | Optional. Token counts still appear in `rimz stats` when cost is 0. |
| `cursor` | Any JSON value; RimZ returns it unchanged on the next request. |
| `origin` | Optional working directory the transcript belongs to. |

### Account

The account probe gets empty stdin and prints one JSON object:

```json
{"plan":"pro","account_id":"account-123","rate_limit_windows":[{"used_percentage":42,"duration_mins":300,"resets_at":"2026-06-01T17:00:00Z","source":"authoritative"}]}
```

| Field | Meaning |
| --- | --- |
| `plan` | Plan name shown on the provider label. |
| `account_id` | Identity label for the account. |
| `rate_limit_windows` | Usage windows for the provider dashboard. |
| `logged_out` | `true` reports the user logged out. A response with none of `plan`, `account_id`, and `rate_limit_windows` counts as logged out too. |

Each window uses the fields listed in [`rimz providers` window fields](./cli/providers.md#json-output): `used_percentage` (an integer from 0 to 100), `resets_at`, and either `duration_mins` for a time window or `scope` for a named quota. A `scope` needs both `id` and `label`. Set `source` to `"authoritative"` when the reading comes from the provider's usage API; without it the reading counts as best-effort. A window of the wrong shape, such as a fractional `used_percentage`, invalidates the whole response. A `context` event from a live session may send the same windows in `rate_limits`.

A named quota with no fixed length carries a `scope` with a stable `id` and a short `label` (three cells fit the dashboard slot) and no `duration_mins`:

```json
{"plan":"business","account_id":"account-123","rate_limit_windows":[{"scope":{"id":"build_minutes","label":"bld"},"used_percentage":42,"resets_at":"2026-06-01T17:00:00Z","source":"authoritative"}]}
```

Without `duration_mins`, the dashboard shows the reset time but no refill, burn pace, or not-started state. A window with both `scope` and `duration_mins` is a sub-cap: it draws as a tick on the unscoped window of the same duration, and not at all when that window is absent. The [provider dashboard](../interface/sidebar.md#the-provider-dashboard) shows how bars and ticks read.

```json
{"logged_out":true}
```

### Version

The version probe gets empty stdin. Its first non-empty stdout line is the agent version.

## Coverage and doctor

`rimz coverage` lists every valid plugin after the built-in agents, with claims derived from the manifest. Run `rimz agents check` for the counts; the [agent support](./agent-support.md) page defines what each claim means.

| Claim | Wired when |
| --- | --- |
| Turn lifecycle | `emits` has `session_start`, `turn_start`, and `turn_end` |
| Permission, plan approval, question | `emits` has `awaiting_input` and `native-ask-ui = true` |
| Compaction | `emits` has `compaction_start` and `compaction_end` |
| Subagents | `subagents = true` and `emits` has `subagent_start` and `subagent_end` |
| Session end | `emits` has `session_end` |
| Context usage | `context-usage = true`, or `emits` has `context` |
| Realtime cost, rich context | `emits` has `context` |
| Account spend | a spend probe is declared |
| Idle notification | always partial, derived from `turn_end` |
| Answering prompts, hook install, remote control, tool statistics, launch reminders, background parking | never |

`rimz doctor` prints an `AGENT PLUGINS` section when any bundle exists, one row per manifest:

| Status | Detail |
| --- | --- |
| `valid` | The setup document's path. |
| `valid; probe unavailable` | `check <probes>`, naming each declared probe that is missing or not executable. |
| `invalid` | The manifest error. |

An invalid bundle anywhere under `agents.d` is a failed precondition for a room: `rimz start` refuses before creating anything, lists every manifest error, and names `rimz agents register --check`. Elsewhere the broken kind is absent. `rimz hooks feed --source <kind>` for it exits 0 with no output when stdin is JSON, so the agent's hooks keep working while you fix the manifest.

## Protocol version

`protocol = 1` in the manifest and `"protocol": 1` in each envelope name the only version RimZ reads. A manifest declaring another version is invalid, and an envelope carrying another version is dropped.

## See also

- [Agent control CLI: register a third-party kind](./cli/agents.md#register-a-third-party-kind): where `register` and `check` sit among the `rimz agents` verbs.
- [Agent support](./agent-support.md): the capability and wiring matrices plugins appear in.
- [Agent plugin internals](../internals/agents/plugin.md): how RimZ loads bundles, decodes envelopes, and runs probes.
