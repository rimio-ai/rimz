# Agent plugins

Agent plugins let an external agent CLI join RimZ without compiling provider-specific Rust into RimZ. A plugin bundle declares identity, branding, launch behavior, emitted events, transcript discovery, and optional pull probes; an agent-side shim translates its native protocol into one canonical JSON envelope.

Plugins are machine configuration and may execute the launch and probe commands they declare. Install only bundles you trust. Project configuration does not register plugins, so plugin commands stay outside the project trust hash.

## Register a bundle

Create a valid skeleton under `$XDG_CONFIG_HOME/rimz/agents.d/<kind>/`:

```sh
rimz agents register mybot
$EDITOR "${XDG_CONFIG_HOME:-$HOME/.config}/rimz/agents.d/mybot/agent.toml"
rimz agents register --check
rimz doctor
```

`rimz agents register` creates `agent.toml`, `README.md`, `shim.sh`, and stub probe executables. RimZ loads every `agents.d/*/agent.toml` once per process. A malformed manifest is skipped on read-only and hook paths; `rimz start` refuses before room side effects and names the manifest and fix.

The complete runnable [ScriptBot example](../../examples/agent-plugin/README.md) supplies a fake agent, canonical shim, priced spend probe, account probe, and fixture transcript.

## Validate a plugin

Use `rimz agents check <kind>` as the authoring loop after each manifest, shim, or probe change:

```sh
rimz agents check mybot
rimz agents check mybot --spend-file ~/.mybot/sessions/sess-123.jsonl
rimz agents check mybot --replay ./canonical-envelopes.jsonl
```

The command validates the named manifest and prints its derived coverage and lifecycle summary. It checks every declared probe for presence and executable permission, then runs the account and version probes; the spend probe runs only with `--spend-file` because its canonical request needs a real transcript path.

`--replay` reads one canonical envelope per JSONL line, runs the same diagnostic parser, hook classifier, lifecycle observer, and pure state machine as ingestion, and prints each event's signal and resulting state plus the final agent states. A malformed envelope exits non-zero with its line and exact parse reason. An event absent from `emits` remains valid but prints a warning, matching live ingestion's permissive behavior.

## Bundle anatomy

```text
agents.d/mybot/
├── agent.toml
├── README.md
├── shim.sh
└── probes/
    ├── spend
    └── account
```

Hook installation is self-managed because RimZ has no native configuration writer for an unknown agent. `setup-doc` points `rimz doctor` at the bundle's installation instructions. The shim owns native-to-canonical translation and invokes `rimz hooks feed --source <kind>` with one envelope on stdin. Hook stdout stays empty: RimZ expresses no decision for a plugin ask, and the agent's own UI owns the answer.

## Manifest

The protocol-1 schema is:

```toml
protocol = 1
kind = "mybot"
display-name = "MyBot"
process-names = ["mybot", "node"]
emits = ["session_start", "turn_start", "turn_end", "context"]
setup-doc = "README.md"

[brand]
emblem = "[mb]"                 # optional; defaults to the shared fallback
color = 141                     # terminal 256-color index
color-rgb = [175, 135, 255]

[capabilities]
native-ask-ui = true            # awaiting_input leaves a prompt in the agent UI
subagents = false
background-tasks = false
registers-lazily = false
context-usage = true

[tools]
mutating = ["write", "shell"]
editing = ["write"]             # must be a subset of mutating

[launch]
bin = "mybot"
args = []
model-flag = "--model"
effort-flag = "--effort"
resume = ["mybot", "--resume", "{session_id}"]
compact-command = "/compact"

[launch.permission-args]
ask = []
auto = ["--auto"]
yolo = ["--yolo"]
plan = ["--plan"]

[transcripts]
globs = ["~/.mybot/sessions/*.jsonl"]
thread-key = "per-file"         # or session-dir

[probes]
spend = ["./probes/spend"]
account = ["./probes/account"]
version = ["mybot", "--version"]
```

`kind` matches `[a-z0-9-]+`, starts and ends with a letter or digit, matches its directory name, and does not collide with a built-in or another plugin. `session_start` is required because it establishes the session row. Optional tables tolerate unknown keys for forward compatibility; protocol version, identity, event names, tool subsets, probe commands, and resume placeholders validate strictly.

Relative probe executables and relative transcript globs resolve from the bundle directory. A launch or resume executable containing a path separator also resolves from the bundle directory; bare executable names use `PATH`. Launch presets fail when a configured `model`, `effort`, or system-prompt field has no declared rendering, so launch intent is not silently discarded.

`emits` is the shim's conformance declaration. RimZ derives the coverage and lifecycle matrices from it plus declared probes and capabilities. The feed still processes a valid canonical event omitted from `emits` and warns, because the live event is authoritative; update `emits` so coverage remains truthful.

## Canonical event envelope

Every envelope is one JSON object with `protocol: 1`, `hook_event_name`, and a root `session_id`. All other fields are optional. A subagent event uses the parent's id as `session_id` and the distinct child's id as `agent_id`; missing, equal, or blank ids quarantine that child event.

Shared enrichment fields may ride every event:

```json
{
  "protocol": 1,
  "hook_event_name": "turn_start",
  "session_id": "sess-123",
  "cwd": "/work/project",
  "model": "mybot-pro",
  "effort": "high",
  "context_pct": 42,
  "context_window": 200000,
  "total_tokens": 84000,
  "input_tokens": 1200,
  "output_tokens": 300,
  "cache_read_input_tokens": 40000,
  "cache_write_input_tokens": 2000,
  "total_cost_usd": 1.25,
  "rate_limits": { "windows": [] },
  "transcript_path": "/home/me/.mybot/sessions/sess-123.jsonl",
  "prompt": "repair the parser"
}
```

The event vocabulary maps directly onto RimZ lifecycle signals:

| Event | Event fields | Effect |
| --- | --- | --- |
| `session_start` | none | Register the root session. |
| `turn_start` | `prompt?` | Start a turn and retain the sanitized prompt as its description. |
| `turn_end` | `errored?`, `error_message?`, `last_assistant_message?` | Finish or fail the turn and complete supervised output. |
| `tool_use` | `tool_name?`, `is_error?` | Record tool activity; manifest tool tables classify mutation and file editing. |
| `awaiting_input` | `ask: permission | plan_approval | question`, `question?` | Mark an agent with native ask UI as waiting. |
| `compaction_start` | none | Open the compaction head. |
| `compaction_end` | `trigger?: auto | manual` | Close compaction and resume or rest according to the trigger. |
| `subagent_start` | child `agent_id` | Register a child linked to the parent `session_id`. |
| `subagent_end` | child `agent_id`, `errored?` | Resolve the child as success or failure. |
| `session_end` | none | Tombstone the session. |
| `context` | shared fields plus normalized context fields | Merge rich display-only context without a lifecycle transition. |

`context` also accepts the optional fields in the serialized [`AgentContext`](../../crates/rimz/src/agents/context.rs) shape, including `session_name`, `session_preview`, `model_id`, `model_display_name`, `thinking_enabled`, `output_style`, `vim_mode`, `agent_version`, `cost`, `tokens`, `rate_limits`, `pr`, and `account`. RimZ stamps `source` and `observed_at`; the shim omits them. Top-level `model`, token split, gauge, cost, and rate-limit fields normalize into the same shape.

Unknown event names and event payloads from a different protocol version are dropped at debug level. A known event with malformed typed fields is also dropped. This keeps a newer shim forward-compatible while preventing malformed input from mutating a row. `rimz agents check --replay` exposes those otherwise-quiet parser decisions while leaving ingestion behavior unchanged.

## Shim contract

The smallest shim forwards an already-canonical payload:

```sh
#!/bin/sh
set -eu
event=${1:?canonical event name}
exec rimz hooks feed --source mybot --event "$event"
```

The agent integration calls the shim with a JSON envelope on stdin. Native ask handling remains inside the agent. RimZ returns success with empty stdout for every plugin event, meaning no opinion; define and test what an empty response means in the native hook system before setting `native-ask-ui = true`.

## Probe contracts

Probes run as best-effort enrichment with the bundle directory as their working directory. RimZ pipes stdin, stdout, and stderr, limits stdout to 1 MiB, and kills a probe that exceeds three seconds. A missing executable, nonzero exit, timeout, oversized output, or invalid JSON omits the enrichment and writes a warning to stderr; it never invalidates durable lifecycle state.

### Spend

RimZ sends one request per discovered transcript file:

```json
{"file":"/home/me/.mybot/sessions/sess-123.jsonl","cursor":{"line":18}}
```

The first request carries `cursor: null`. Return entries after that opaque cursor and a replacement cursor:

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

The cursor is arbitrary JSON and round-trips unchanged. Timestamps use RFC 3339. Plugin entries are already priced: RimZ uses `cost_usd` verbatim and does not apply its built-in price book. Token components remain useful when cost is absent or zero because `rimz stats` still reports token composition.

### Account

The account probe receives an empty stdin stream and returns either a login or an authoritative logout:

```json
{"plan":"pro","account_id":"account-123","rate_limit_windows":[{"used_percentage":42,"duration_mins":300,"resets_at":"2026-06-01T17:00:00Z","source":"authoritative"}]}
```

A provider-defined quota can use a stable scope instead of a duration:

```json
{"plan":"business","account_id":"account-123","rate_limit_windows":[{"scope":{"id":"build_minutes","label":"bld"},"used_percentage":42,"resets_at":"2026-06-01T17:00:00Z","source":"authoritative"},{"scope":{"id":"deployments","label":"dep"},"used_percentage":7,"resets_at":"2026-06-01T17:00:00Z","source":"authoritative"}]}
```

```json
{"logged_out":true}
```

`plan` feeds the provider label. `account_id` is the plugin's provider identity label. Optional `rate_limit_windows` use the normalized `RateLimitWindow` shape and feed the shared account-usage cache; canonical `context` events may also push the same windows while a session is live. A scoped window's non-empty `scope.id` is its stable fusion/cache identity and its `scope.label` is a compact presentation label clipped safely to the three-cell provider-bar slot. Omit `duration_mins` when the provider exposes no trustworthy duration: the reset may display, but rolling refill, burn pace, surplus, not-started detection, and priming remain disabled. Existing duration-only responses retain their current identity and labels unchanged.

### Version

The version probe receives an empty stdin stream. Its first non-empty stdout line becomes the provider version.

## Coverage, doctor, and failure behavior

`rimz coverage` includes every valid plugin after the built-ins. Native rows come from `emits`; context and ask claims also require the matching capability; spend comes from the declared probe; hook installation and remote control remain explicitly unsupported.

`rimz doctor` shows each manifest, its validation result, the setup document, and whether every declared probe exists and is executable. `rimz start` treats a malformed configured plugin as a failed precondition. Hook feed treats that same broken bundle as unavailable and exits neutrally, keeping the agent's critical hook path open while the doctor and start error retain the fix.

Protocol version 1 is stable. Additive optional fields and new events require a future-compatible reader; a semantic change to an existing field or event requires a new integer protocol version.

The protocol is the canonical envelope vocabulary delivered to RimZ. `rimz hooks feed` is the version-1 delivery mechanism, not part of the vocabulary: the envelopes contain no exec-specific assumptions, so a future resident receiver can accept the same objects over a socket without changing their fields or event semantics.
