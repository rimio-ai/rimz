# The agent harness

> See [DESIGN.md](../../../DESIGN.md) for the commitments this doc operationalizes. The agent *model* — the rollup, state machine, turn phase, liveness, and adapter boundary — is [agent.md](./agent.md); this doc owns the machinery around it: spawning the fleet, addressing it, driving it with `steer` and `queue`, the supervised runs automation drives, and the cleanup that reclaims its panes. The worktrees that back its channels are [worktree.md](./worktree.md).

One agent in one thread is a conversation. Tens of agents across a dozen worktrees is a team: some you start by hand, others a cron job, a PR-review trigger, a CI gate, or a script kicks off, and they cooperate — Claude drafts the plan while Codex reviews it as peer programming. The harness runs that team the way you run an engineering org. You open a channel for each line of work, you reach any member by name, you talk to one live or leave it a task for when it is free, and automation joins the room as just another teammate.

Everything a team needs has a primitive here, and the rest of this doc is those primitives end to end.

| You're running a team and you need… | The harness gives you | The move |
| --- | --- | --- |
| a channel for each line of work | a worktree — the sidebar groups the room by it | `rimz agents … --worktree=<name>` |
| members who do the work | agents, each addressable by handle | `@claude`, `@codex`, `@planner` |
| to name one member precisely | the address `@handle#channel` | `@codex#feat-x`, `@claude-2`, `@all` |
| to talk to someone right now | `steer` — type into a live pane | `rimz steer @claude -- "…"` |
| to leave a task for when they're free | `queue` — deliver at the next open turn | `rimz queue @codex --on done -- "…"` |
| to add a member or open a channel | spawn — agents × layout × worktree | `rimz agents peer --worktree=feat/x` |
| automation to drive a member | a supervised headless run | `rimz agents codex -p "…"` (cron · CI · PR) |
| to read the room and catch up | cards, listings, captures | `agents list` / `show` / `wait`, `pane capture`, `queue list` |

Everything here rides primitives both backends share: the layout compiles to backend-neutral panes, placement to a tab or a split, addressing to one shared parser, and messaging to the one pane-send primitive humans and resolvers already use.

## The model

Spawning the fleet separates three independent choices, so any combination is one command: **agents** choose which tools run, **layout** chooses the shape on screen, and **worktree** chooses which channel they run in. `claude,codex` plus `--worktree=feat/x` puts a planner and a reviewer side by side in one channel; the same agents with a different layout or a different worktree is the same three knobs turned differently.

Three words carry the whole model:

- A **channel** is a [worktree](./worktree.md) — one copy of the code where a few members cooperate. The sidebar groups the room by it, and an address narrows to it with `#<channel>`.
- A **member** is an agent, named by a **handle**: `@claude` the kind, `@planner` the profile, `@swift-otter` the one running instance.
- An **address** joins them — `@handle#channel` — and is how every command names who it is reaching.

You reach a member two ways, because a team works in two tenses. **Steer** talks to a live pane now. **Queue** leaves a task that delivers at the member's next open turn. Both name their target with the same address and ride the same pane-send primitive.

```text
one room, grouped into channels — one per worktree

  #feat-auth   @claude    planning       @codex  reviewing
  #deps        @codex     -p run (from CI)
  #docs        @planner   queued: "draft the API"

reach a member by @handle#channel, then:
  steer @claude  →  talk to it now
  queue @codex   →  leave a task for its next open turn
```

## How you use it

Open a channel with a planner and a reviewer, address them by profile or kind, and drive them in either tense:

```sh
rimz agents peer --worktree=feat/x      # Claude planning beside Codex reviewing, in #feat-x
rimz steer @claude -- "focus on the failing parser test"   # talk to the live pane now
rimz queue @codex --on done -- "open a PR summary"         # leave a task for its next idle turn
```

Let automation drive a member with no room attached — the supervised `-p` run is the door cron, CI, and PR hooks come through:

```sh
rimz agents codex --worktree=deps --timeout 4h -p "update dependencies, run the suite, open a PR"
```

Reach a member that does not exist yet and create it from the address; read the room when you come back:

```sh
rimz steer @planner#feat/x --create -- "draft the API"     # opens a planner in #feat-x, this as its first prompt
rimz agents list                        # the cards, addressed by channel
rimz agents show @codex#deps            # one member, with live context when active
```

The rest of this doc follows a member through its life: how a spawn becomes panes, the address that names it, the talk-and-queue path that reaches it, the supervised run that drives it headless, and the cleanup that reclaims its pane. The channels members work in — Rimz-owned worktrees — are [worktree.md](./worktree.md).

## Spawn the fleet

### The layout IR

`rimz agents <spec>` resolves either a named `[agents.layouts]` entry or an inline DSL. Commas split columns, plus signs stack rows within a column, and each cell is a profile, command, or built-in cell: built-in `term`, an agent kind, an adapter-supported virtual `<kind>-<mode>` / `<kind>-ping` variant such as `claude-auto` or `codex-yolo`, an `[agents.profiles]` entry, or an `[agents.commands]` entry ([configuration.md](../../reference/configuration.md#agent-profiles-commands-and-layouts)).

```text
claude,codex+term
vim,htop+zsh
claude-auto,codex-yolo
```

The first example creates two columns: Claude on the left, Codex stacked above a shell on the right. The second creates raw command panes from user commands. The third opens agent cells with adapter-owned permission posture args. The built-in `peer` layout is `claude,codex`; bare `rimz agents` lists cards, while the hidden layout default remains one `term` cell for internal callers.

The CLI converts cells to backend-neutral `LayoutPanes`: an agent cell runs the hidden `rimz agents exec <kind>` wrapper with optional `--prompt`, optional `--worktree-path`, optional `--agent-profile`, and `-- <args>` from its profile; a command cell runs its raw argv, with empty argv reserved for the user's shell. Backends never resolve agent kinds or worktrees — the wrapper does. It runs the agent command in the pane and inherits the pane's TTY, launching the agent through the user's default shell startup path when that shell and `/usr/bin/env` are available (re-applying Rimz launch env after shell rc/profile files) and falling back to direct exec for unsupported or missing shells. The wrapper is the seam the supervised-run and cleanup paths below hang off.

### Backend shape and placement

Each backend renders the same compiled layout into a tab, and a single-cell launch can split the current view instead.

tmux opens a window with `new-window -d -P`, lets the session's `after-new-window` hook dock the sidebar once, and adds the remaining layout cells with `split-window`. Columns use horizontal splits; rows use vertical splits anchored inside their column.

Zellij renders a temporary KDL layout for `new-tab --layout`: the global sidebar pane on the left, one pane per column to the right, nested horizontal splits for stacked rows, and the compact bar restored at the bottom.

Both backends receive the same `TabOptions`: session, title, cwd, focus flag, sidebar options, and the pre-built pane argv. A worktree launch names the tab `⑂ <NAME>` (the worktree name behind the worktree glyph); a launch without a worktree names the tab `<kind>:<dir>`. `--bg` keeps focus on the launching pane where the backend can do so.

Placement follows intent. A single non-worktree launch can land in the current view instead of a fresh tab: the CLI then calls `split_pane` with the one cell's argv rather than `open_tab`, reusing the launching pane's sidebar and pinning the new pane to the room through the shared launch-identity env. The per-machine `[agents] tab` default and the per-launch `--same-tab` / `--new-tab` flags choose between the two paths ([configuration.md](../../reference/configuration.md#agent-profiles-commands-and-layouts)). Under the `auto` default a worktree launch or a multi-cell layout opens its own tab, while a single non-worktree agent splits the current view; an explicit `--same-tab` (or `tab = "same"`) also splits a single worktree launch into the current view, while a multi-cell layout always opens its own tab. Placement resolves before the launch touches the ledger or creates a worktree, so a rejected `--same-tab` leaves no provisional rows or worktree behind. `split_pane` carries the launch-identity env on both backends — tmux through `-e`, Zellij through an `env` command prefix — and honors the same focus flag, with tmux dropping `-d` to land in the new pane and Zellij returning focus to the launching pane when `--bg` holds it back.

## The address

Every member in a room has an address you type like an @-mention in a channel: `@<handle>#<channel>`. The handle names who; the channel names where. Both read from context — `@claude` uses the channel you are in, and `#auth` alone filters a listing to that channel.

The channel is the workspace segment the room already groups by: a worktree branch, else a child repo's directory name, else the directory itself — the grouping the sidebar shows ([sidebar.md → Worktree groups](../sidebar/sidebar.md#worktree-groups)). It matches by branch, path basename, or full path, and defaults to the channel the command runs in; an inline `#<name>` or `--worktree` overrides it. A bare directory workspace has no current channel, so an address there reaches every channel rather than silently narrowing to one. Mux tab names stay display-only — they are mutable and live outside the ledger, so they never form an address.

Handles come in two kinds. A **type handle** names a profile or kind to fill and carries enough to launch one:

- `@<kind>` — `@codex`, the agent kind. Matches every agent of that kind in the channel, including those launched under a profile.
- `@<profile>` — `@planner`, an `[agents.profiles]` profile ([configuration.md](../../reference/configuration.md#agent-profiles-commands-and-layouts)). Matches every agent launched under that profile.

An **instance handle** names one running agent and only ever addresses what already exists:

- `@<petname>` — `@swift-otter`, the stable per-agent name (set at launch with `--name`).
- `@<kind>-<ordinal>` — `@claude-2`, the nth agent of a kind in the channel.
- a session-id prefix.
- `<mux>:<pane>` — `tmux:%1`, a precise, channel-agnostic pane address.

`@all` is the broadcast handle: every agent in the channel.

The rendered handle is the shortest address that names exactly that agent, and it round-trips through the parser. Rimz renders it profile-first — the profile when it is unique in scope, else the kind, else `@<kind>-<n>`, else the petname — so a listing always shows the handle you could type back. A handle appears only when typing it reaches that one agent, so two `planner`s in a channel each render as their kind ordinal — `@claude-1` / `@claude-2` — and every handle you see resolves to exactly one agent. One canonical renderer, the inverse of the parser, is shared by every agent-bearing listing (`agents list`, `agents show`, `queue list`, the channel headers); [target.rs](../../../crates/rimz/src/target.rs) owns it.

An address resolves to zero, one, or many agents, and arity decides the outcome:

| Matches | Outcome |
| --- | --- |
| one | delivered |
| many | an ambiguity error listing the handles to pick one, unless `--all` (or the explicit `@all`) opts into the fan-out |
| zero | a miss that names where the agent runs in another channel, or — with `--create` — launches it |

When a type handle matches several agents, the address resolves to an ambiguity that lists the handles to pick one — `rimz steer @codex` with two codexes stops there. `--all` (or `@all`) opts into the fan-out, which confirms before sending unless `-y` skips it; a blocked agent in a fan-out skips while the rest still send.

`--create` launches a missing agent straight from its address. `rimz steer @planner#feat/x --create -- "draft the API"` opens a `planner` in `#feat/x` — creating the worktree when the channel is new — with the text as its first prompt. Only a type handle creates, because only a kind or a profile carries what a launch needs; an instance handle (a petname, ordinal, or session id) names something that must already exist and refuses with the fix. Create-on-miss is the same launch as `rimz agents <kind|profile> --worktree=<channel> "<prompt>"`, reached from the address.

## Talk and queue

Rimz delivers text to a live member now (`rimz steer`) or at its next open delivery point (`rimz queue`). Both ride the same pane-send primitive humans and resolvers share, address agents through the [address grammar](#the-address) above, and take state decisions from the ledger snapshot and the hook lifecycle. The two mirror each other — the same address, fan-out, `--force`, `--no-enter`, `--auto-compact`, `--file`, and `--no-from` surface, and the same `\n` soft-newline text — and diverge only on timing: `queue` adds `--on`, the gate that picks the boundary to deliver at.

### Targets

`steer` and `queue` require the `@` sigil — a bare selector fails with a `did you mean @…?` hint — so a stray word never broadcasts by accident; a pane id is the one sigil-free exception. They resolve the [address](#the-address) against a freshly produced snapshot, so a just-started pane is present.

Floating Zellij panes participate in `steer` live-pane addressing while the sidebar room renders tiled panes.

The two commands address different layers, because they deliver at different times. `steer` reaches **live panes**: a bare `@<kind>` or `@all` also reaches a pane that has not bound a session yet — a lazy-registering agent (Codex) before its first turn ([agent.md](./agent.md#the-instance-lifecycle)) — because the address a paste needs is the pane, which the producer already detects for that pane's idle row. `queue` keys a durable record on a session id, so it addresses **bound sessions**; an address that matches only an unbound pane has no key, so `queue` points it at `steer` to start the session first. A petname, kind ordinal, or session-id prefix names a bound session under either command.

Fan-out and `--create` follow the [address rules](#the-address) above: more than one match needs `--all` (or `@all`) and confirms unless `-y`, and one blocked or paneless agent skips while the rest still send.

### Steer

`rimz steer <target> -- <text>` injects into each resolved pane immediately as a [bracketed paste](#bracketed-paste-submit) and then presses Enter as a discrete keystroke outside the paste — the submit — while any `\n` inside the text rides the paste as a soft composer newline, so a multi-line prompt lands multi-line. The CLI interprets the two-character `\n` escape in `<text>` (and `\\` for a literal backslash; every other escape keeps its backslash, so a regex or path survives), so a newline can be typed inline without shell quoting. `--file <PATH>` reads the prompt from a file in place of `<text>` and sends it verbatim — no `\n`/`\\` interpretation, since a file already holds real newlines and literal backslashes — refusing both an inline `<text>` alongside it and an empty or unreadable file. A Rimz-launched agent sends with `@sender: ` prepended; a cross-channel send prepends `@sender#channel: `. `--no-from` keeps the delivered text exact. `--no-enter` types the text and holds the Enter. A pending feed ask attached to a bound agent skips that agent; `--force` records the override and sends anyway. The `agent.steered` event records kind, pane id, force flag, sender address when present, and text length per send, plus the session id when one is bound — an unbound pane records only kind and pane. Message content stays out of the event log.

### Bracketed-paste submit

Both `steer` and `queue` delivery wrap the text in bracketed-paste markers (`ESC[200~` … `ESC[201~`) through the `MuxBackend::paste_text` primitive, then press Enter as a separate `send_key`. Agent composers run paste-detection heuristics: text and a trailing `\r` coalesced into one PTY read are taken as pasted content, and the `\r` becomes a literal newline rather than a submit. The paste markers make the boundary lexical — the composer leaves paste mode on `ESC[201~`, so the following Enter is unambiguously a keystroke even when every byte arrives in one read. The generic `rimz pane send` stays on the raw type path, since a bare shell would render the markers literally.

### Compact before sending

`--auto-compact <PCT|TOKENS>` lands a message against a fresh window: when the agent's context fill has reached the threshold, Rimz submits the agent's `/compact` first, then the message, so the prompt runs after the compaction instead of racing the agent's own auto-compaction mid-turn. The threshold is a percentage of the window (`70%`) or an occupied-token count (`120000`), compared against the live fill — the folded statusline reading where present, else the per-call token split, else the carried gauge. An unknown fill is not a full window, so it sends the message untouched.

The compaction is the agent's own slash command, owned by the adapter (`AgentAdapter::compact_command` — `/compact` for every wired agent). It rides the raw type path, not [the bracketed paste](#bracketed-paste-submit): a composer treats pasted text as literal content, so a pasted `/compact` would land as a prompt rather than run. `steer` reads the fill now and compacts before the immediate paste; `queue` re-reads it at the delivery boundary and types `/compact` ahead of the message in the same delivery, so a failed compaction fails the delivery through the same retry path as a failed send. A lazy pane with no bound session carries no fill, so `steer --auto-compact` to one simply sends.

### Queue: leave a task for later

A queued message waits for the member to be free and delivers itself at the next open turn. `rimz queue <target> -- <text>` enqueues one; `rimz queue list` shows pending and terminal records by canonical handle and sender; `rimz queue remove <msg-id>` drops one pending message and `rimz queue clear <target>` drops every pending message for a member.

Queued messages live under the workspace state root:

```text
queue/<msg_id>.json
queue/terminal/<msg_id>.json
```

`msg_` ids are UUIDv7, so filename order is FIFO order. Pending scans read only `queue/*.json`; claimed and final records move atomically into `queue/terminal/`. The directory is created lazily, so a workspace with no queued messages costs the hook path one missing-dir stat.

Each record stores the workspace id, agent kind, agent session id, sender identity, text, Enter flag, delivery gate, force flag, status, enqueue/update timestamps, attempt count, last attempt timestamp, last error, and delivered timestamp. Status values are `pending`, `claimed`, `delivered`, `removed`, and `abandoned`.

### Gates

`--on done` opens when the rollup status is `idle` or `success`. `--on any` also opens on `failed`. `running`, `waiting`, and `paused` keep delivery closed. A pending ask attached to the agent keeps delivery closed for every gate, because the next input belongs to that ask — unless the message was queued with `--force`, which delivers past the ask, mirroring `steer --force`.

The queue requires installed and trusted hooks for the target agent. Hooks are the delivery signal; accepting a queue entry for an unwired agent would create durable work with no transition that can release it.

### Delivery

Only unparked root turn ends trigger delivery. `Registered`, subagent stops, compaction events, and parked background turn ends do not check the queue. The lifecycle hook records the event, then spawns a detached `rimz queue deliver --message-id <id>` helper with nulled stdio for the FIFO head.

The helper waits `400ms` by default (`RIMZ_QUEUE_SETTLE_MS` overrides this for tests), reads the pending head, checks a fresh snapshot for the gate, the pending-ask predicate (skipped when the record is `--force`), and the bound pane, computes the sender prefix against the target's current channel, then claims the head under the workspace lock immediately before sending. State misses leave the message pending for a later transition. The claim moves the record to `claimed`, outside the pending scan, and increments the attempt count. A successful send moves the record to `delivered`; a send failure records `last_error` and returns it to `pending`, and after five attempts the record becomes `abandoned`. The claim timestamp throttles retries after a send failure. A crash after claim leaves a visible `claimed` record that `queue list` surfaces; it is not auto-redelivered on a later turn end.

Delivery is FIFO per agent, and one message is attempted per unparked root turn end.

### Audit events

Queue writes append `message.queued`, `message.delivered`, `message.removed`, and `message.abandoned` events — `remove` and `clear` both append `message.removed`. Events include message id, kind, agent id, sender address when present, gate, status, text length, Enter flag, attempt count, and reason. They never include message text.

`rimz gc` abandons open messages whose `(kind, agent_id)` no longer appears in the current rollup. This is maintenance, not delivery; normal state misses stay pending.

### Hazards

Queued text can still land while a human has half-typed a draft in the agent pane. Rimz gates on ledger state, not focused-pane state or captured composer contents.

Agent UIs can present dialogs that are not represented as feed asks. Core keeps pane capture out of message delivery; resolvers that need to inspect UI text own capture-before-send.

Multiplexer sends are best-effort. A pane can disappear or reject input after the claim. The queue records the error and retries on future turn-end transitions until the attempt cap.

## Supervised runs

When a cron job, a CI gate, a PR hook, or a script needs to drive one member and read its result, it uses a supervised run. `rimz agents <spec> <prompt> -p` launches one interactive agent pane, waits for the agent's root turn to end, prints the final assistant message or stream events, and exits with a script-friendly status code: `0` completed, `1` failed, `124` timed out, `130` canceled. It is the headless entry point: a script drives one agent turn and reads the result, with no sidebar and no attached client.

**Run records and completion.** A run record is written under `runs/<run_id>.json` before the pane opens, the launched `rimz agents exec` wrapper exports `RIMZ_RUN_ID`, and lifecycle hooks fold matching root-session observations into that record. The wrapper also records its own normalized pane id when the mux exposes one, so cleanup can close the launched pane without waiting for the sidebar snapshot to bind the agent session. The first root `TurnEnded` completes the run as `completed` or `failed`; a session `Ended` before a turn result marks it failed; `rimz agents stop <run-id>` marks an active run `canceled`; subagent events and same-kind descendant processes with a different session id are ignored, so child completions never finish the parent command. If the wrapper observes the agent process exit and no terminal lifecycle hook lands after a short grace, it writes `failed` and wakes the waiter, making process death a liveness backstop without treating pane exit as success.

Supervised `-p` runs require installed and trusted hooks, because hooks provide the completion signal ([agent.md → Hook install](./agent.md#hook-install--the-visible-security-step)).

Auto-ping rides this path: a scheduled `rimz autoping run` drives one lowest-effort `ping`→`pong` supervised turn to start a provider's budget window on your schedule ([autoping.md](./autoping.md)).

**The wakeup socket.** The blocking CLI binds `sock/run.<short_id>.sock` before opening the pane. When a hook, timeout, or operator stop writes the first terminal run record it sends a `run_completed` wakeup frame to that socket; the record on disk remains truth, and the datagram only cuts latency. If the wait cap expires, the CLI reloads the record once to catch a just-written terminal result, otherwise writes `timed_out` and exits `124`.

**Output and input formats.** `--output-format` chooses the projection `-p` prints: `text` (default) prints the final assistant message, `json` prints the terminal run record, and `stream-json` emits NDJSON run events as the turn runs. `--input-format` chooses the prompt source: `text` (default) reads the positional prompt, while `stream-json` reads user messages from stdin until EOF (a bare `content` string or the `text` of each content block), refusing a positional prompt. `stream-json` output is incompatible with `--detach`.

**Streaming is transcript-tail based.** Run records store the adapter transcript path when lifecycle observations provide it; if the first lifecycle hook arrives before the transcript file exists, the first later observation that carries a path writes it once. `rimz agents <spec> <prompt> -p --output-format stream-json` and `rimz agents wait <run-id> --stream` poll that file with the torn-write-safe cursor used for transcript reads, parse only newly appended assistant messages through the selected adapter, and reset the cursor if the transcript path changes. Stream status events come from the same read-time join as `agents show`; the run socket still exists only to wake a blocking producer promptly. An attached `agents wait --stream` timeout stops the watcher and exits `124` without mutating the run record.

**Permission posture is adapter-owned.** The run chooses `auto` (the default), `--ask`, or `--yolo`; adapters translate that into provider CLI arguments. Claude maps `auto` to accepted edits and `yolo` to its dangerous bypass flag. Codex maps `auto` to `--ask-for-approval never --sandbox workspace-write`, leaves `ask` at the provider default, and maps `yolo` to its dangerous approvals-and-sandbox bypass.

**Shared launch params are adapter-owned the same way.** `--effort` and `--system-prompt-file` render through each adapter's `render_preset` — the one place per-agent native launch flags are built — so `--effort high` becomes Claude's `--effort high` or Codex's `-c model_reasoning_effort=high`, and `--system-prompt-file` becomes Claude's `--system-prompt-file` or Codex's `-c model_instructions_file=`. An adapter with no native flag for a param refuses the launch, naming the unsupported flag, rather than dropping the intent.

**Durability and inspection.** Run records are cold-path durable state and use temp-file-plus-rename with fsync through the ledger atomic helpers. `rimz agents show <run-id>` reads the current workspace's retained run records and attaches live card context when the run is still active. Live fields stay out of the durable run record, so clearing and agent drift do not create extra locked writes. Records are retained until an operator removes state.

## Cleanup

When an agent exits, the same `rimz agents exec` wrapper that launched it reclaims what it owns: the supervised run's pane, and — for a worktree launch — the worktree.

**Worktree cleanup.** An agent launched with `--worktree-path` triggers worktree cleanup on exit, which proves the branch's work landed before removing the tree and deleting its branch. The cleanup helper, the decision table, and the `rimz gc` sweep are [worktree.md → Cleanup](./worktree.md#cleanup).

**Run pane cleanup.** After a blocking `-p` run finishes, pane cleanup is best-effort: Rimz closes the recorded launch pane when available, then falls back to finding the agent row by `(kind, agent_id)` in the snapshot. A detached run (`--detach`) passes cleanup ownership to the in-pane wrapper: unless `--keep` was set, the wrapper watches the run record, terminates the agent process once the run is terminal, performs marked-worktree cleanup, and closes its own pane. Operator stops use the same terminal record and wakeup path, then check whether the recorded pane remains after a short grace and close it through the mux backend when possible; `--keep` controls natural-completion cleanup, not an explicit stop.
