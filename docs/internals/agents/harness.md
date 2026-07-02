# The agent harness

> See [DESIGN.md](../../../DESIGN.md) for the commitments this doc operationalizes. The agent *model* — the rollup, state machine, turn phase, liveness, and adapter boundary — is [agent.md](./agent.md); the message system and its channel lanes are [message.md](./message.md); Git worktree backing is [worktree.md](./worktree.md); the user-facing commands are [cli/agents.md](../../reference/cli/agents.md). This doc owns the machinery between them: spawning the fleet, addressing it, the supervised runs automation drives, the scheduled loop tasks that drive those runs on a clock, and the cleanup that reclaims its panes.

One agent in one thread is a conversation; tens of agents across a dozen worktrees is a team. The harness runs that team. It spawns agents into panes, reaches any one by name, drives it live or leaves it a task for when it is free, and reclaims its pane when it exits — the same machinery whether a human, a cron job, a CI gate, or a PR hook is doing the driving.

Everything here rides primitives both backends share: a layout compiles to backend-neutral panes, placement lands on a tab or a split, an address resolves through one parser, and a message rides the one pane-send primitive humans and resolvers already use. [cli/agents.md](../../reference/cli/agents.md) is the command surface (flags, synopses, examples); this doc is what those commands do underneath.

## The model

Spawning the fleet separates three independent choices, so any combination is one command: **agents** choose which tools run, **layout** chooses the shape on screen, and **channel** chooses the cooperation lane they run in. `claude,codex` plus `--channel=design` or `--worktree=feat/x` puts a planner and a reviewer side by side in one channel; the same agents with a different layout or channel is the same three knobs turned differently.

Three words name the parts:

- A **channel** is one cooperation lane where a few members work together, backed by a durable bare name, a [worktree](./worktree.md), an in-place named team as `<dir>/<team>`, or the directory room. The sidebar groups the room by it, and an address narrows to it with `#<channel>`.
- A **member** is an agent, named by a **handle**: `@claude` the kind, `@planner` the profile, `@swift-otter` the one running instance.
- An **address** joins them (`@handle#channel`): it is how every command names who it reaches.

You reach a member through `message`. `--steer` talks to a live pane now; the default talks now when the member can receive and parks a task when the member needs a later turn boundary; `--schedule` sets an earliest delivery time before that boundary can open. Every mode names its target with the same address and rides the same pane-send primitive.

```text
one room, grouped into channels — named lanes, worktrees, teams, directories

  #feat-auth   @claude    planning       @codex  reviewing
  #design      @planner   outlining
  #deps        @codex     -p run (from CI)
  #docs        @planner   queued: "draft the API"

reach a member by @handle#channel, then:
  message --steer @claude  →  talk to it now
  message @codex           →  talk now if free, otherwise leave a task
  message --schedule 1h    →  leave a task no earlier than one hour from now
```

## Spawn the fleet

### The layout IR

`rimz agents <spec>` resolves either a named `[agents.teams]` entry or an inline DSL, and both compile to the same backend-neutral panes. The inline grammar is compact: commas split columns, plus signs tile rows within a column, slashes stack rows within a column on Zellij, and each cell is a built-in `term`, an agent kind, a virtual `<kind>-<mode>` / `<kind>-ping` variant (`claude-auto`, `codex-yolo`), a configured profile, or a configured command ([configuration.md](../../reference/configuration.md#agent-profiles-commands-and-teams)). A named team is an ordered role list that opens as one side-by-side column per role unless it declares its own `layout`, which uses the same grammar and resolves declared role names before falling through to roleless cells. A named team also accepts `<team>.<role>` to launch one declared role with its team identity. That single role places like any single agent: in the current pane by default, back to the shell on exit, or a fresh tab when no launching pane exists.

```text
claude,codex+term      → Claude left; Codex tiled over a shell right
claude/codex/term      → two agents plus a shell in one Zellij stack; tmux tiles them
vim,htop+zsh           → raw command panes
claude-auto,codex-yolo → agent cells with adapter-owned permission posture
```

Stacks are presentation only: Zellij renders a native stack with one expanded pane, while tmux keeps the same cells as tiled rows because it has no native stack.

The compile target is the seam the whole harness hangs off. Each cell becomes a `LayoutPanes` entry. An agent cell runs the hidden **`rimz agents exec <kind>`** wrapper, carrying the prompt, worktree path, profile, role, model, effort, and `-- <args>` resolved from the profile and role; a command cell runs its raw argv, with an empty argv reserved for the user's shell. Backends never resolve agent kinds or worktrees: the wrapper does. It runs the agent in the pane, inheriting the pane's TTY, launching through the user's shell-startup path when that shell and `/usr/bin/env` are available and falling back to direct exec otherwise. It exports `RIMZ_RTK` from `[harness] rtk` into the run, which `cargo xtask` reads to route recognized cargo subcommands through `rtk`. Because the wrapper stays resident, it is also where the supervised-run and cleanup paths below attach.

### Backend shape and placement

Each backend renders the same compiled layout into a tab, and a single non-worktree cell can run in the current pane instead. Both backends receive the same `TabOptions` — session, title, cwd, focus flag, sidebar options, and the pre-built pane argv — and dock the global sidebar once before adding the layout cells; the per-backend split commands live in [`mux/`](../../../crates/rimz/src/mux/AGENTS.md). A named-channel or worktree launch names its tab `#<NAME>`, matching the channel suffix in agent addresses; a named team launch names it `team:<name>`, and its in-place channel is `<dir>/<team>`; any other non-worktree launch names it `<kind>:<dir>`. `--bg` keeps focus on the launching pane wherever the backend can.

**Placement resolves before the launch touches the ledger or creates a worktree**, so a rejected placement leaves no provisional rows or worktree behind. Under the `auto` default a single non-worktree cell launches *in the current pane*: the CLI execs the wrapper argv in place, the wrapper binds the pane and direct-execs the agent, and the pane returns to its shell on exit with liveness resolved from the pane rather than an end trace. A named-channel launch, multi-cell layout, or worktree launch opens its own tab. `--new-pane` splits the current tab, `--new-tab` opens a tab, and the per-machine [`[agents] placement`](../../reference/configuration.md#agent-profiles-commands-and-teams) default chooses when no flag is given. `--bg` and create-on-miss downgrade an in-place launch to a split, because the caller's pane stays available; the split carries the launch-identity env on both backends and honors the same focus flag.

## The address

Every member has an address you type like an @-mention: `@<handle>#<channel>`. The handle names who, the channel names where, and both read from context — `@claude` uses the channel you are in, `#auth` alone filters a listing to that channel. [cli/agents.md → Addressing agents](../../reference/cli/agents.md#addressing-agents) is the handle catalog; this section owns how an address resolves.

The **channel** is the workspace segment the room already groups by: an explicit named channel, else a worktree branch from the agent's resolved git root, else an in-place team as `<dir>/<team>`, else the directory itself ([message.md § Channels](./message.md#channels)). It matches by explicit name, branch, path basename, full path, or team channel, and defaults to the channel the command runs in; an inline `#<name>`, `--channel`, or `--worktree` overrides it. A bare directory workspace has no current channel for humans, so an address there reaches *every* channel rather than silently narrowing to one; named-channel panes carry `RIMZ_CHANNEL`, and team member panes carry `RIMZ_TEAM`. Mux tab names stay display-only — they are mutable and live outside the ledger, so they never form an address.

A **handle** falls into three classes, narrowing from group to instance:

- A **role handle** (`@coder`) names a team role and matches every agent launched under it in the channel. Role names reserve built-in kind handles so kind addresses keep round-tripping.
- A **type handle** names a kind (`@codex`) or a profile (`@planner`) and matches every agent of it in the channel. It carries enough to launch one, so only a type handle can create.
- An **instance handle** names one running agent and only ever addresses what exists: a petname (`@swift-otter`), a kind ordinal (`@claude-2`), a session-id prefix, or a precise `<mux>:<pane>` pane address. `@all` is the broadcast handle for the whole channel.

The rendered handle is the shortest address that names exactly that agent, and it round-trips through the parser. Rimz renders it role-first — the role when unique in scope, then the profile when unique, else the kind, else `@<kind>-<n>`, else the petname — so a listing always shows a handle you could type back, and a handle appears only when typing it reaches that one agent. One canonical renderer, the inverse of the parser, is shared by every agent-bearing listing; [target.rs](../../../crates/rimz/src/target.rs) owns both.

An address resolves to zero, one, or many agents against a fresh snapshot, and arity decides the outcome:

| Matches | Outcome |
| --- | --- |
| one | delivered |
| many | an ambiguity error listing the handles to pick one, unless `--all` or `@all` opts into fan-out; fan-out delivers to every match, prefixes each delivery with the addressed handle (`@all,`, `@claude,`) so receivers read it as a group message, and skips a blocked agent while the rest send |
| zero | a miss that names where the agent runs in another channel and lists live agents, or — with `--create` — launches it |

`--create` launches a missing agent straight from its address: `rimz message --steer @planner#design --create "draft the API"` opens a `planner` in `#design`, registering the named channel, with the text as its first prompt. With `--worktree feat/x`, create-on-miss creates or reuses the worktree instead. Only a type handle creates, because only a kind or profile carries what a launch needs; an instance handle names something that must already exist and refuses with the fix.

## Talk and queue

The message system — send modes, the durable message record, delivery gates and FIFO ordering, the hook-triggered delivery pipeline, scheduling, smart compaction, wait confirmation, retries, and the audit trail — lives in [message.md](./message.md). The user-facing command surface is in [cli/agents.md § Message an agent](../../reference/cli/agents.md#message-an-agent).

## Supervised runs

When a cron job, CI gate, PR hook, or script needs to drive one member and read its result, it uses a supervised run. `rimz agents <spec> <prompt> -p` opens one interactive agent pane (splitting the current tab by default, a new tab only with `--new-tab` or outside a room), waits for the agent's root turn to end, prints the result, and exits with a script-friendly code: `0` completed, `1` failed, `124` timed out, `130` canceled. Automation drives one agent turn without attaching to the room; an in-room caller sees the transient pane beside the current one. Supervised runs require installed and trusted hooks, because hooks are the completion signal ([agent.md → Hook install](./agent.md#hook-install--the-visible-security-step)). Rimz's built-in scheduler drives this same path on a clock ([Scheduled turns](#scheduled-turns-loop)).

**Run records and completion.** A run record is written under `runs/<run_id>.json` before the pane opens, the launched wrapper exports `RIMZ_RUN_ID`, and lifecycle hooks fold matching root-session observations into it. The wrapper also records its own normalized pane id, so cleanup can close the launched pane without waiting for the snapshot to bind the session. The first root `TurnEnded` completes the run `completed` or `failed`; a session `Ended` before a turn result marks it failed; `rimz agents stop <run-id>` marks an active run `canceled`; subagent events and same-kind descendants with a different session id are ignored, so a child completion never finishes the parent. If the wrapper observes the agent process exit and no terminal hook lands after a short grace, it writes `failed` and wakes the waiter — process death is the liveness backstop, and pane exit is never read as success.

**The wakeup socket.** The blocking CLI binds `sock/run.<short_id>.sock` before opening the pane. The first terminal run record sends a `run_completed` datagram to that socket; the record on disk stays truth and the datagram only cuts latency. If the wait cap expires, the CLI reloads the record once to catch a just-written terminal result, otherwise writes `timed_out` and exits `124`.

**Output and input formats.** `--output-format` chooses the projection `-p` prints (`text` the final assistant message, `json` the run record, `stream-json` NDJSON run events as the turn runs); `--input-format` chooses the prompt source (`text` the positional prompt plus piped stdin, `stream-json` user messages from stdin until EOF). Streaming is **transcript-tail based**: run records store the path to the agent's own provider-native transcript, not the Rimz transcript log that `rimz transcript` renders. `--output-format stream-json` and `agents wait --stream` poll that path with the torn-write-safe cursor used for transcript reads, parse only newly appended assistant messages through the selected adapter's `parse_transcript_messages`, and reset the cursor if the path changes. The run socket still exists only to wake a blocking producer promptly.

**Posture and launch params are adapter-owned.** A run chooses `auto` (default), `--ask`, or `--yolo`, and `--model` / `--effort` / `--system-prompt-file` / `--append-system-prompt-file` render through each adapter's `render_preset` — the one place per-agent native launch flags are built. An adapter with no native flag for a param refuses the launch, naming the unsupported flag, rather than dropping the intent (supervised `--max-turns` renders through a separate per-adapter turn-limit hook). The provider-specific mappings live in the adapter docs.

**Durability and inspection.** Run records are cold-path durable state, written with temp-file-plus-rename through the ledger atomic helpers and retained until an operator removes state. `rimz agents show <run-id>` reads the retained records and attaches live card context while the run is active; live fields stay out of the durable record, so clearing and agent drift create no extra locked writes.

## Scheduled turns (loop)

`rimz loop` runs a turn on a clock. The room's elected sidebar elder — the producer node ([state.md → The node model](../sidebar/state.md#the-node-model)) — keeps time while a room for the task's project is open, and on its data tick fires `rimz loop run <name>` for every task that has come due. A task drives one of three actions: `spec` spawns one transient supervised pane down the [supervised-run](#supervised-runs) path, `bind` delivers a prompt to one live agent through the [message](./message.md) path, and `check` runs a shell command that either stands alone or guards one of the other two. Everything below is what the elder and the hidden `rimz loop run` do underneath; the command surface — flags, synopses, examples — is [cli/agents.md → Schedule turns with loop](../../reference/cli/agents.md#schedule-turns-with-loop).

A `spec` task names exactly one agent cell: a built-in kind, a profile, or an adapter-supported virtual cell such as `claude-auto`, `codex-yolo`, or `claude-ping`. Teams, multi-cell layouts, and command cells are rejected at add time, because a scheduled task owns one supervised pane.

### Schedule forms

`rimz loop add` validates the task, runs hook preflight when it carries an agent action, and makes it live immediately while a room for its project is open. Durable recurring definitions live in per-machine `loop.toml`; Rimz-generated ephemerals live in state (below). A task carries one firing shape:

- **Calendar** — `at = "07:00"` with an optional `days` mask (`daily`, `weekdays`, `weekends`, a range like `mon-fri`, or a list `mon,wed,fri`). Wall-clock evaluation uses the configured `timezone`, falling back to the system zone when unset.
- **Interval** — `every = "15m"`, `2h`, or `1d`. The elder fires at the exact interval measured from the last arm or fire.
- **Raw cron** — `cron = "*/15 * * * *"`, matched by an in-process five-field matcher over minute, hour, day-of-month, month, and day-of-week in the configured `timezone`.
- **One-shot** — `once = true` on a calendar or cron schedule. `rimz loop add --in 30m` resolves to an `at` time in the configured `timezone` and implies `once`.
- **Poll-until** — `every = "2m"` with `check`, `on`, an agent action, and `deadline`. `rimz loop add --until 30m` stores the resolved absolute deadline in instance state.

An ephemeral task — a one-shot, or any task with a `deadline` — removes its own state row before the supervised run or delivery. A one-shot removed pre-fire that then fails to launch is not retried. A poll-until task also removes itself when its check fires the agent action, and expires without delivery once its `deadline` passes.

### Elder firing

The elder keeps a per-room `loop-fire.json` map of task name to last-fire `Timestamp` under the workspace runtime dir. First sight arms a task by recording `now` and does not fire; the next matching occurrence fires. A fire records `now` before spawning the detached helper, so a hot sub-interval tick cannot spawn the same occurrence twice.

Arming stamps the first-sight time, and that stamp sets the firing edge each schedule reads. A calendar task fires at the first tick at or after its wall-clock time on a matching day, at most once that day — so a tick a few seconds late still fires it, but a task first seen *after* its time today waits for the next matching day. A cron task fires only on a tick whose minute matches, so a room opened past a matching minute waits for the next match. An interval task fires once the measured elapsed time crosses the interval.

Each room fires only tasks whose normalized `root` maps to its `WorkspaceId`. `rimz loop add` writes a canonical absolute root; a hand-edited `~` or relative root is expanded and canonicalized before the ownership check, display, and execution.

The elder spawns `rimz loop run <name>` with fresh null stdio. That hidden runner resolves the recorded root, runs any `check` first, applies agent hook preflight only when the guard fires, and then launches the supervised pane or messages the pinned session.

Self-paced loops are ordinary one-shots. An agent schedules its next wake with `--in <delay>` at the end of the current wake; the instance row is removed before delivery, so the agent creates the next one only while it still has work. The pending wake stays visible in `rimz loop list` without editing `loop.toml`.

### Script checks

`check = "<shell>"` runs through `sh -c` at the task's project root before any agent action. `on = "fail"` — the default — wakes on a non-zero exit or a timeout; `on = "success"` wakes on a zero exit. `timeout = "5m"` bounds the check, falling back to five minutes when unset.

A check-only task is a scheduled command with no agent action: it logs `completed`, `failed`, or `timed out` and keeps recurring unless it is ephemeral. A guarded task logs `skipped` when the command exits with the non-firing polarity; when the guard fires, Rimz appends the command, its exit status, and the capped combined output to the base prompt before spawning or delivering.

Two patterns fall out of the guard. The watchdog runs a command on a schedule and wakes an agent on failure (`every = "15m"`, `check = "cargo test"`, `on = "fail"`, `spec = "codex"`). The trigger-when-green polls until a command succeeds, then delivers (`every = "2m"`, `check = "gh run watch --exit-status"`, `on = "success"`, `bind = ...`, `deadline = ...`). A poll-until instance stops in one of two cases: the first matching check result fires the agent action, or the `deadline` passes and the run logs `expired`.

Script checks are per-machine user automation, like a personal crontab. `loop.toml` lives outside the repository and outside the project trust hash, so a clone cannot supply a check command; project trust hashes only the executable fields of `.rimz/config.toml` ([trust.md](../sidebar/trust.md)).

### Delivering to a live instance

`bind` pins a schedule to one exact agent session. `rimz loop add <name> --bind @<handle> ...` resolves the address against the live rollup at add time, records a `bind` sub-table of `kind`, `session`, and `handle`, and rejects `spec` and supervised-run flags because delivery opens no pane.

On fire, the runner resolves the recorded `root`, confirms the pinned root session still exists, and sends the prompt through the same [message](./message.md) path as `rimz message`, gated `done`. An idle agent receives it immediately; a running agent parks it for its next `done` turn boundary; a missing session is skipped and the schedule removed, because that exact conversation cannot return. `rimz gc` runs the same liveness check and reaps bind schedules whose pinned session has left the rollup — a safety sweep for tasks that never fired successfully after the agent exited.

### Window-priming pings

A task whose `spec` is a `<kind>-ping` virtual cell starts a provider's budget window at a time you choose. It defaults `prompt = "ping"` and lowest effort unless configured otherwise, and the virtual cell supplies the adapter's ping arguments. The window is account-scoped, shared by every session of a provider kind ([provider.md → Window fusion](./provider.md#window-fusion)), so one ping per provider primes the whole account. Before spawning the turn, the runner reads the shared rate-limit cache and skips when the shortest window is already counting down. The read is best-effort: an unknown or cold cache falls through to the ping, since missing a window-start defeats the feature while an occasional extra token is cheap.

### State and code

Durable definitions live in `~/.config/rimz/loop.toml` under `[tasks.*]`. Machine-generated one-shots, self-wakes, and poll-until instances live in `~/.local/state/rimz/loop-instances.json` with the same task shape; `is_ephemeral = once || deadline.is_some()` routes a task between the two on add and drives removal-on-fire. Per-room arm/fire stamps live in runtime `loop-fire.json`, and user-global run history lives in state `loop-runs.log.jsonl`.

- [`schedule.rs`](../../../crates/rimz/src/schedule.rs) — pure parsing, descriptions, and due evaluation.
- [`cli/loop_cmd.rs`](../../../crates/rimz/src/cli/loop_cmd.rs) — config and state editing, the `list` surface, and the hidden `run` runner, including check execution and prompt augmentation.
- [`loop_instances.rs`](../../../crates/rimz/src/loop_instances.rs) — the ephemeral state store.
- [`loop_fire.rs`](../../../crates/rimz/src/loop_fire.rs) — elder firing and the `loop-fire.json` state.
- [`loop_run_log.rs`](../../../crates/rimz/src/loop_run_log.rs) — result history, including `check_skipped` and `expired`.

## Cleanup

When an agent exits, the same `rimz agents exec` wrapper that launched it reclaims what it owns: the supervised run's pane, and — for a worktree launch — the worktree.

**End traces.** Interactive agent panes keep the wrapper resident. A clean child exit is deliberate. A signal exit from a tab/pane close is deliberate when the mux session still exists, even if the room is mid-teardown or missing sidebar chrome. The wrapper records a durable `agent.ended` trace before slower cleanup, so that agent stays out of future recovery. When the mux session itself is gone at wrapper exit — reboot, mux crash, closing the last tab, in-`start` stuck-room recovery — the wrapper preserves recovery state so resume birth can regroup the agent into a `#<channel>` tab.

**Worktree cleanup.** An agent launched with `--worktree-path` triggers worktree cleanup on deliberate exit, which proves the branch's work landed before removing the tree and deleting its branch. Clean quits keep the existing interactive helper attached to the pane. Signal exits start the helper with null stdio in its own process group, so cleanup can finish after the closing pane disappears. The helper, decision table, and `rimz gc` sweep are [worktree.md → Cleanup](./worktree.md#cleanup).

**Run-pane cleanup.** After a blocking `-p` run finishes, pane cleanup is best-effort: Rimz closes the recorded launch pane, falling back to finding the agent row by `(kind, agent_id)` in the snapshot. A detached run (`--detach`) passes cleanup to the in-pane wrapper: unless `--keep` was set, the wrapper watches the run record, terminates the agent once the run is terminal, performs marked-worktree cleanup, and closes its own pane. An operator stop uses the same terminal record and wakeup path, then closes the recorded pane if it lingers past a short grace — reclaiming a kept run's pane whether the ref is the run id or the agent name.
