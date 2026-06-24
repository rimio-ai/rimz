# The agent harness

> See [DESIGN.md](../../../DESIGN.md) for the commitments this doc operationalizes. The agent *model* — the rollup, state machine, turn phase, liveness, and adapter boundary — is [agent.md](./agent.md); the worktrees that back its channels are [worktree.md](./worktree.md); the user-facing commands are [cli/agents.md](../../reference/cli/agents.md). This doc owns the machinery between them: spawning the fleet, addressing it, driving it with `steer` and `queue`, the supervised runs automation drives, and the cleanup that reclaims its panes.

One agent in one thread is a conversation; tens of agents across a dozen worktrees is a team. The harness runs that team. It spawns agents into panes, reaches any one by name, drives it live or leaves it a task for when it is free, and reclaims its pane when it exits — the same machinery whether a human, a cron job, a CI gate, or a PR hook is doing the driving.

Everything here rides primitives both backends share: a layout compiles to backend-neutral panes, placement lands on a tab or a split, an address resolves through one parser, and a message rides the one pane-send primitive humans and resolvers already use. [cli/agents.md](../../reference/cli/agents.md) is the command surface — flags, synopses, examples; this doc is what those commands do underneath.

## The model

Spawning the fleet separates three independent choices, so any combination is one command: **agents** choose which tools run, **layout** chooses the shape on screen, and **worktree** chooses which channel they run in. `claude,codex` plus `--worktree=feat/x` puts a planner and a reviewer side by side in one channel; the same agents with a different layout or worktree is the same three knobs turned differently.

Three words name the parts:

- A **channel** is a [worktree](./worktree.md), or an in-place named team as `<dir>/<team>` — one cooperation lane where a few members work together. The sidebar groups the room by it, and an address narrows to it with `#<channel>`.
- A **member** is an agent, named by a **handle**: `@claude` the kind, `@planner` the profile, `@swift-otter` the one running instance.
- An **address** joins them — `@handle#channel` — and is how every command names who it is reaching.

You reach a member in two tenses. **Steer** talks to a live pane now; **queue** talks now when the member can receive, and parks a task only when the member needs a later turn boundary. Both name their target with the same address and ride the same pane-send primitive.

```text
one room, grouped into channels — one per worktree

  #feat-auth   @claude    planning       @codex  reviewing
  #deps        @codex     -p run (from CI)
  #docs        @planner   queued: "draft the API"

reach a member by @handle#channel, then:
  steer @claude  →  talk to it now
  queue @codex   →  talk now if free, otherwise leave a task
```

## Read the room

`rimz transcript` is the catch-up surface, and the only part worth noting here is the fusion. A single-agent target groups that agent's local transcript into turns; a channel target (`#channel`, `@all#channel`, or a bare invocation in a worktree) reads every root agent in the channel and fuses their messages into one timestamp-ordered timeline labelled by handle. Codex rollout rows that omit timestamps inherit the last timestamp seen in their session file, so sparse progress rows stay anchored to their turn. The parser core is shared with supervised streaming: each adapter implements `parse_transcript_messages` once, and the assistant-only `wait --stream` path filters that same parse.

## Spawn the fleet

### The layout IR

`rimz agents <spec>` resolves either a named `[agents.teams]` entry or an inline DSL, and both compile to the same backend-neutral panes. The inline grammar is compact: commas split columns, plus signs stack rows within a column, and each cell is a built-in `term`, an agent kind, a virtual `<kind>-<mode>` / `<kind>-ping` variant (`claude-auto`, `codex-yolo`), a configured profile, or a configured command ([configuration.md](../../reference/configuration.md#agent-profiles-commands-and-teams)). A named team is an ordered role list that opens as one side-by-side column per role unless it declares its own `layout`, which uses the same grammar and resolves declared role names before falling through to roleless cells. A named team also accepts `<team>.<role>` to launch one declared role with its team identity; under the default placement this splits beside the caller or opens a tab when no launching pane exists, so recovering a member never takes over the pane issuing the command.

```text
claude,codex+term      → Claude left; Codex stacked over a shell right
vim,htop+zsh           → raw command panes
claude-auto,codex-yolo → agent cells with adapter-owned permission posture
```

The compile target is the seam the whole harness hangs off. Each cell becomes a `LayoutPanes` entry that runs the hidden **`rimz agents exec <kind>`** wrapper — carrying the prompt, worktree path, profile, role, model, effort, and `-- <args>` resolved from the profile and role — or, for a command cell, its raw argv (empty argv reserved for the user's shell). Backends never resolve agent kinds or worktrees: the wrapper does. It runs the agent in the pane, inheriting the pane's TTY, launching through the user's shell-startup path when that shell and `/usr/bin/env` are available and falling back to direct exec otherwise. Because the wrapper stays resident, it is also where the supervised-run and cleanup paths below attach.

### Backend shape and placement

Each backend renders the same compiled layout into a tab, and a single non-worktree agent can run in the current pane instead. Both backends receive the same `TabOptions` — session, title, cwd, focus flag, sidebar options, and the pre-built pane argv — and dock the global sidebar once before adding the layout cells; the per-backend split commands live in [`mux/`](../../../crates/rimz/src/mux/AGENTS.md). A worktree launch names its tab `#<NAME>`, matching the channel suffix in agent addresses; a named team launch names it `team:<name>`, and its in-place channel is `<dir>/<team>`; any other non-worktree launch names it `<kind>:<dir>`. `--bg` keeps focus on the launching pane wherever the backend can.

**Placement resolves before the launch touches the ledger or creates a worktree**, so a rejected placement leaves no provisional rows or worktree behind. Under the `auto` default a single non-worktree agent launches *in the current pane*: the CLI execs the wrapper argv in place, the wrapper binds the pane and direct-execs the agent, and the pane returns to its shell on exit with liveness resolved from the pane rather than an end trace. A team, multi-cell layout, or worktree launch opens its own tab. `--new-pane` splits the current tab, `--new-tab` opens a tab, and the per-machine [`[agents] placement`](../../reference/configuration.md#agent-profiles-commands-and-teams) default chooses when no flag is given. `--bg` and create-on-miss downgrade an in-place launch to a split, because the caller's pane stays available; the split carries the launch-identity env on both backends and honors the same focus flag.

## The address

Every member has an address you type like an @-mention: `@<handle>#<channel>`. The handle names who, the channel names where, and both read from context — `@claude` uses the channel you are in, `#auth` alone filters a listing to that channel. [cli/agents.md → Addressing agents](../../reference/cli/agents.md#addressing-agents) is the handle catalog; this section owns how an address resolves.

The **channel** is the workspace segment the room already groups by: a worktree branch, else a child repo's directory name, else the directory itself; an in-place named team appends its team name as `<dir>/<team>` ([sidebar.md → Worktree groups](../sidebar/sidebar.md#worktree-groups)). It matches by branch, path basename, full path, or team channel, and defaults to the channel the command runs in; an inline `#<name>` or `--worktree` overrides it. A bare directory workspace has no current channel for humans, so an address there reaches *every* channel rather than silently narrowing to one; team member panes carry `RIMZ_TEAM`, so their own `rimz` calls default to `<dir>/<team>`. Mux tab names stay display-only — they are mutable and live outside the ledger, so they never form an address.

A **handle** falls into three classes, narrowing from group to instance:

- A **role handle** (`@coder`) names a team role and matches every agent launched under it in the channel. Role names reserve built-in kind handles so kind addresses keep round-tripping.
- A **type handle** names a kind (`@codex`) or a profile (`@planner`) and matches every agent of it in the channel. It carries enough to launch one, so only a type handle can create.
- An **instance handle** names one running agent and only ever addresses what exists: a petname (`@swift-otter`), a kind ordinal (`@claude-2`), a session-id prefix, or a precise `<mux>:<pane>` pane address. `@all` is the broadcast handle for the whole channel.

The rendered handle is the shortest address that names exactly that agent, and it round-trips through the parser. Rimz renders it role-first — the role when unique in scope, then the profile when unique, else the kind, else `@<kind>-<n>`, else the petname — so a listing always shows a handle you could type back, and a handle appears only when typing it reaches that one agent. One canonical renderer, the inverse of the parser, is shared by every agent-bearing listing; [target.rs](../../../crates/rimz/src/target.rs) owns both.

An address resolves to zero, one, or many agents against a fresh snapshot, and arity decides the outcome:

| Matches | Outcome |
| --- | --- |
| one | delivered |
| many | an ambiguity error listing the handles to pick one, unless `--all` (or `@all`) opts into the fan-out — which confirms before sending unless `-y`, and skips a blocked agent while the rest send |
| zero | a miss that names where the agent runs in another channel, or — with `--create` — launches it |

`--create` launches a missing agent straight from its address: `rimz steer @planner#feat/x --create -- "draft the API"` opens a `planner` in `#feat/x`, creating the worktree when the channel is new, with the text as its first prompt. Only a type handle creates, because only a kind or profile carries what a launch needs; an instance handle names something that must already exist and refuses with the fix. Create-on-miss is exactly the launch `rimz agents <kind|profile> --worktree=<channel> "<prompt>"` would run, reached from the address.

## Talk and queue

`steer` and `queue` both deliver text to a member, ride the same pane-send primitive humans and resolvers share, resolve the [address](#the-address) above against a fresh snapshot, and take their state decisions from the ledger and the hook lifecycle. They mirror each other on flags ([cli/agents.md](../../reference/cli/agents.md#steer-live-agents) is the surface) and diverge on one thing: timing. `queue` sends through the steer path when the target can receive now; otherwise it parks a durable record, and `--on` picks the later boundary that opens that record.

### Targets

The two commands read both live panes and durable agent cards. `steer` reaches **live panes**: a bare `@<kind>` or `@all` also reaches a pane that has not bound a session yet — a lazy-registering agent (Codex) before its first turn ([agent.md → The instance lifecycle](./agent.md#the-instance-lifecycle)) — because the thing a paste needs is the *pane*, which the producer already detects. `queue` uses that live pane when the target can receive now, including lazy panes with no session yet; when it must park work, it keys the durable record on the bound session or launch placeholder card so FIFO survives registration. A petname, kind ordinal, or real session-id prefix names a bound session under either command; launch placeholder ids stay internal. (Floating Zellij panes participate in live-pane addressing.)

The `@` sigil is required — a bare selector fails with a `did you mean @…?` hint, so a stray word never broadcasts; a pane id is the one sigil-free exception.

### Steer

`rimz steer <target> -- <text>` injects into each resolved pane immediately as a [bracketed paste](#bracketed-paste-submit), writes a durable message record, then presses Enter as a discrete keystroke *outside* the paste — the submit — while any `\n` inside the text rides the paste as a soft composer newline, so a multi-line prompt lands multi-line. By default a Rimz-launched agent's send arrives prefixed `from @sender: `, gaining `#channel` when it crosses channels; `--no-from` delivers the bytes exact. A pending feed ask attached to a bound agent skips that agent unless `--force` records the override and sends anyway. The `message.sent` event records metadata — message id, kind, session, pane, force flag, sender, text length, and status — never the message content.

### Bracketed-paste submit

Both commands wrap the text in bracketed-paste markers (`ESC[200~` … `ESC[201~`) through `MuxBackend::paste_text`, then press Enter as a separate `send_key`. This makes the boundary lexical: agent composers run paste-detection heuristics — text plus a trailing `\r` coalesced into one PTY read is taken as pasted content, with the `\r` a literal newline rather than a submit — so the composer leaves paste mode on `ESC[201~` and the following Enter is unambiguously a keystroke even when every byte arrives in one read. The generic `rimz pane send` stays on the raw type path, since a bare shell would render the markers literally.

The discrete writes land one second apart after the first write: paste immediately, wait, submit. This gives a busy composer separate paste and submit events on the PTY.

### Compact before sending

`--smart-compact <PCT|TOKENS>` lands a message against a fresh window: when the agent's context fill has reached the threshold, Rimz sends a tracked `/compact` command message first, waits one message interval, then sends the prompt message, so the prompt runs after compaction instead of racing the agent's own auto-compaction mid-turn. The threshold is a percentage of the window or an occupied-token count, compared against the live fill (the folded statusline reading where present, else the per-call token split, else the carried gauge); an omitted flag falls back to the [`[harness] smart_compact`](../../reference/configuration.md#smart-compaction) default, and an unknown fill is not a full window, so it sends untouched.

The compact-first path paces `/compact`, its submit, the message, and its submit one second apart after the first write, so compaction settles before the message arrives.

The compaction is the agent's own slash command, owned by the adapter (`AgentAdapter::compact_command`). It rides the raw type path, **not** the bracketed paste — a composer treats pasted text as literal content, so a pasted `/compact` would land as a prompt rather than run. `steer` and send-now `queue` read the fill just before the immediate paste; parked queue records store the threshold and re-read fill at the delivery boundary, typing `/compact` ahead of the message in the same delivery so a failed compaction fails the delivery through the same retry path as a failed send.

### Queue: leave a task for later

A queue command sends immediately like `steer` when the target has a live pane, the gate is open, no pending ask reserves input unless `--force`, and no older queued message owns that card's FIFO head. That send writes the same durable `sent` record and `message.sent` event as `steer`. A target that is busy, gated, blocked by a pending ask, missing a live pane, or behind FIFO gets a parked message under the workspace state root:

```text
queue/<msg_id>.json            queued
queue/terminal/<msg_id>.json   claimed, sent, and final
```

`msg_` ids are UUIDv7, so filename order is FIFO order; queued scans read only `queue/*.json`, and the directory is created lazily so an empty workspace costs the hook path one missing-dir stat. Each record stores the workspace, kind, session id or launch placeholder id (or a pane-derived placeholder for a lazy send-now pane), sender, body (`prompt` or `command`), text, Enter flag, gate, force flag, pane id when known, status (`created` transient → `queued` → `claimed` → `sent` → `delivered`, or `timed_out` / `errored` / `removed` / `abandoned`), timestamps, and attempt bookkeeping. The full record is the field catalog; the lifecycle below is the contract.

### Gates

`--on` picks the boundary to open for parked records: `done` opens when the rollup status is `idle` or `success`, `any` also opens on `failed`, and `running` / `waiting` / `paused` keep delivery closed. A pending ask attached to the agent keeps delivery closed for every gate — the next input belongs to that ask — unless the message was queued with `--force`, mirroring `steer --force`. Installed and trusted hooks are required only for the park path, because hooks are the delivery signal: accepting a parked entry for an unwired agent would create durable work no transition could release.

### Delivery

Only **unparked root turn ends** trigger parked delivery — `Registered`, subagent stops, compaction events, and parked background turn ends do not check the queue. The lifecycle hook records the event, then spawns a detached `rimz queue deliver` helper with nulled stdio for the FIFO head. The helper waits a short settle delay (`RIMZ_QUEUE_SETTLE_MS` overrides it for tests), reads the queued head, checks a fresh snapshot for the gate, the pending-ask predicate (skipped under `--force`), and the target's live pane, then claims the head under the workspace lock immediately before sending through the same steer path.

The claim moves the record out of the queued scan and increments the attempt count. A successful pane write moves it to `sent`; the agent's next body-matching lifecycle hook confirms the oldest `sent` record for that card as `delivered` (`prompt` on `TurnStarted`, `command` on `Compacting`). Smart compaction prepends a fresh `command` record at delivery time before the claimed prompt. A pre-send failure records `last_error` and returns it to `queued`, throttled by the claim timestamp, and after the retry cap the record becomes `abandoned`; a failure after bytes were written becomes `errored` to avoid duplicate retry text. A state miss leaves the message queued for a later transition. A crash after claim leaves a visible `claimed` record that `queue list` surfaces; it is not auto-redelivered. Delivery is FIFO per agent, one message per unparked root turn end. Queue writes append `message.queued` / `message.sent` / `message.delivered` / `message.timed_out` / `message.errored` / `message.removed` / `message.abandoned` audit events (metadata only, never text). `rimz gc` abandons open messages whose `(kind, agent_id)` no longer appears in the rollup and times out `sent` records older than `RIMZ_MESSAGE_DELIVERY_WINDOW_MS`.

`--wait[=DURATION]` upgrades `steer` and send-now `queue` from fire-and-return to synchronous confirmation. The command waits until the prompt record reaches `delivered`, `timed_out`, or `errored`, prints `delivered @handle`, `timed out @handle`, or `errored @handle`, and exits nonzero for the latter two. Bare `--wait` uses `RIMZ_MESSAGE_DELIVERY_WINDOW_MS` or the default delivery window. Broadcast waits share one deadline across all prompt records. A smart-compact send owns two message records when it triggers: the `/compact` command confirms on `Compacting`, and the prompt confirms on `TurnStarted`; one cannot confirm the other. `--force` sent mid-turn can time out because a resumed turn emits no fresh `TurnStarted` for that paste. A sessionless lazy pane confirms only after a real session or name can match its pane-derived placeholder record, so the first prompt can time out even when the paste succeeds.

### Hazards

- Queued text can land while a human has half-typed a draft in the agent pane. Rimz gates on ledger state, not focused-pane state or captured composer contents.
- Agent UIs can present dialogs that are not feed asks. Core keeps pane capture out of delivery; a resolver that needs to inspect UI text owns capture-before-send.
- Multiplexer sends are best-effort: a pane can disappear or reject input after the claim, which the queue records and retries until the attempt cap.

## Supervised runs

When a cron job, CI gate, PR hook, or script needs to drive one member and read its result, it uses a supervised run. `rimz agents <spec> <prompt> -p` opens one interactive agent pane (splitting the current tab by default, a new tab only with `--new-tab` or outside a room), waits for the agent's root turn to end, prints the result, and exits with a script-friendly code: `0` completed, `1` failed, `124` timed out, `130` canceled. Automation drives one agent turn without attaching to the room; an in-room caller sees the transient pane beside the current one. Supervised runs require installed and trusted hooks, because hooks are the completion signal ([agent.md → Hook install](./agent.md#hook-install--the-visible-security-step)). Loop tasks ride this path ([loop.md](./loop.md)).

**Run records and completion.** A run record is written under `runs/<run_id>.json` before the pane opens, the launched wrapper exports `RIMZ_RUN_ID`, and lifecycle hooks fold matching root-session observations into it. The wrapper also records its own normalized pane id, so cleanup can close the launched pane without waiting for the snapshot to bind the session. The first root `TurnEnded` completes the run `completed` or `failed`; a session `Ended` before a turn result marks it failed; `rimz agents stop <run-id>` marks an active run `canceled`; subagent events and same-kind descendants with a different session id are ignored, so a child completion never finishes the parent. If the wrapper observes the agent process exit and no terminal hook lands after a short grace, it writes `failed` and wakes the waiter — process death is the liveness backstop, and pane exit is never read as success.

**Launch environment.** The wrapper exports `RIMZ_RTK` from `[harness] rtk`; `cargo xtask` uses it to route recognized cargo subcommands through `rtk` for agent runs.

**The wakeup socket.** The blocking CLI binds `sock/run.<short_id>.sock` before opening the pane. The first terminal run record sends a `run_completed` datagram to that socket; the record on disk stays truth and the datagram only cuts latency. If the wait cap expires, the CLI reloads the record once to catch a just-written terminal result, otherwise writes `timed_out` and exits `124`.

**Output and input formats.** `--output-format` chooses the projection `-p` prints (`text` the final assistant message, `json` the run record, `stream-json` NDJSON run events as the turn runs); `--input-format` chooses the prompt source (`text` the positional prompt plus piped stdin, `stream-json` user messages from stdin until EOF). Streaming is **transcript-tail based**: run records store the adapter transcript path, and `--output-format stream-json` / `agents wait --stream` poll it with the torn-write-safe cursor used for transcript reads, parsing only newly appended assistant messages through the selected adapter and resetting the cursor if the path changes. The run socket still exists only to wake a blocking producer promptly.

**Posture and launch params are adapter-owned.** A run chooses `auto` (default), `--ask`, or `--yolo`, and `--model` / `--effort` / `--system-prompt-file` / `--append-system-prompt-file` render through each adapter's `render_preset` — the one place per-agent native launch flags are built. An adapter with no native flag for a param refuses the launch, naming the unsupported flag, rather than dropping the intent (supervised `--max-turns` renders through a separate per-adapter turn-limit hook). The provider-specific mappings live in the adapter docs.

**Durability and inspection.** Run records are cold-path durable state, written with temp-file-plus-rename through the ledger atomic helpers and retained until an operator removes state. `rimz agents show <run-id>` reads the retained records and attaches live card context while the run is active; live fields stay out of the durable record, so clearing and agent drift create no extra locked writes.

## Cleanup

When an agent exits, the same `rimz agents exec` wrapper that launched it reclaims what it owns: the supervised run's pane, and — for a worktree launch — the worktree.

**End traces.** Interactive agent panes keep the wrapper resident. When the child exits while the mux session is still alive, the wrapper records a durable `agent.ended` trace for the agent stamped on its pane — the tab/pane close path, which keeps that agent out of future recovery. When the room is gone or unhealthy at wrapper exit (reboot, mux crash, closing the last tab, in-`start` stuck-room recovery), the wrapper *suppresses* the trace, so the agent stays recoverable and resume birth can regroup it into a `#<channel>` tab.

**Worktree cleanup.** An agent launched with `--worktree-path` triggers worktree cleanup on exit, which proves the branch's work landed before removing the tree and deleting its branch. The helper, decision table, and `rimz gc` sweep are [worktree.md → Cleanup](./worktree.md#cleanup).

**Run-pane cleanup.** After a blocking `-p` run finishes, pane cleanup is best-effort: Rimz closes the recorded launch pane, falling back to finding the agent row by `(kind, agent_id)` in the snapshot. A detached run (`--detach`) passes cleanup to the in-pane wrapper: unless `--keep` was set, the wrapper watches the run record, terminates the agent once the run is terminal, performs marked-worktree cleanup, and closes its own pane. An operator stop uses the same terminal record and wakeup path, then closes the recorded pane if it lingers past a short grace — reclaiming a kept run's pane whether the ref is the run id or the agent name.
