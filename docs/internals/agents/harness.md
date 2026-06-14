# The agent harness

> See [DESIGN.md](../../../DESIGN.md) for the commitments this doc operationalizes. The agent *model* — the rollup, state machine, turn phase, liveness, and adapter boundary — is [agent.md](./agent.md); this doc owns the machinery around it.

Rimz launches an agent fleet by separating three choices: **agents** choose which tools run, **layout** chooses the shape on screen, and **worktree** chooses where they run. This doc follows one harness agent through its life — the layout that shapes a fleet into panes, the worktree that isolates it, the placement that lands its tab, the supervised `-p` run that drives a single turn from a script, the cleanup that reclaims both when it exits, and the `steer`/`queue` path that types into it while it is live.

Everything here rides primitives both backends share: the layout compiles to backend-neutral panes, placement to a tab or a split, and messaging to the one pane-send primitive humans and resolvers already use.

## The layout IR

`rimz agents <spec>` resolves either a named `[agents.layouts]` entry or an inline DSL. Commas split columns, plus signs stack rows within a column, and each cell is an alias or a built-in cell: built-in `term`, an agent kind, an adapter-supported virtual `<kind>-<mode>` variant such as `claude-auto` or `codex-yolo`, or an `[agents.aliases]` entry ([configuration.md](../../reference/configuration.md#agent-aliases-and-layouts)).

```text
claude,codex+term
vim,htop+zsh
claude-auto,codex-yolo
```

The first example creates two columns: Claude on the left, Codex stacked above a shell on the right. The second creates raw command panes from user aliases. The third opens agent cells with adapter-owned permission posture args. The built-in `peer` layout is `claude,codex`; bare `rimz agents` lists cards, while the hidden layout default remains one `term` cell for internal callers.

The CLI converts cells to backend-neutral `LayoutPanes`: an agent cell runs the hidden `rimz agents exec <kind>` wrapper with optional `--prompt`, optional `--worktree-path`, and `-- <args>` from its alias; a command cell runs its raw argv, with empty argv reserved for the user's shell. Backends never resolve agent kinds or worktrees — the wrapper does. It runs the agent command in the pane and inherits the pane's TTY, launching the agent through the user's default shell startup path when that shell and `/usr/bin/env` are available (re-applying Rimz launch env after shell rc/profile files) and falling back to direct exec for unsupported or missing shells. The wrapper is the seam the supervised-run and cleanup paths below hang off.

## Backend shape and placement

Each backend renders the same compiled layout into a tab, and a single-cell launch can split the current view instead.

tmux opens a window with `new-window -d -P`, lets the session's `after-new-window` hook dock the sidebar once, and adds the remaining layout cells with `split-window`. Columns use horizontal splits; rows use vertical splits anchored inside their column.

Zellij renders a temporary KDL layout for `new-tab --layout`: the global sidebar pane on the left, one pane per column to the right, nested horizontal splits for stacked rows, and the compact bar restored at the bottom.

Both backends receive the same `TabOptions`: session, title, cwd, focus flag, sidebar options, and the pre-built pane argv. A worktree launch names the tab `⑂ <NAME>` (the worktree name behind the worktree glyph); a launch without a worktree names the tab `<kind>:<dir>`. `--no-focus` keeps the current view active where the backend can do so.

Placement follows intent. A single non-worktree launch can land in the current view instead of a fresh tab: the CLI then calls `split_pane` with the one cell's argv rather than `open_tab`, reusing the launching pane's sidebar and pinning the new pane to the room through the shared launch-identity env. The per-machine `[agents] tab` default and the per-launch `--same-tab` / `--new-tab` flags choose between the two paths ([configuration.md](../../reference/configuration.md#agent-aliases-and-layouts)). Under the `auto` default a worktree launch or a multi-cell layout opens its own tab, while a single non-worktree agent splits the current view; an explicit `--same-tab` (or `tab = "same"`) also splits a single worktree launch into the current view, while a multi-cell layout always opens its own tab. Placement resolves before the launch touches the ledger or creates a worktree, so a rejected `--same-tab` leaves no provisional rows or worktree behind. `split_pane` carries the launch-identity env on both backends — tmux through `-e`, Zellij through an `env` command prefix — and honors the same focus flag, with tmux dropping `-d` to land in the new pane and Zellij returning focus to the launching pane when focus is held back.

## Rimz-owned worktrees

`rimz worktree new` creates a Git worktree under the per-machine `[worktree] dir` template, defaulting to a sibling `../{repo}-worktrees/<name>`, and creates a branch named `<name>` from the configured base (`head`, `fresh`, or an explicit ref). The marker stores the base branch name and the resolved base commit snapshot, so cleanup measures committed work against the live base branch and keeps the snapshot as the detached or unresolved fallback. Omitted names come from a two-word generated name; explicit names use letters, numbers, `_`, and `-`.

The checkout stays clean of Rimz metadata. Ownership lives in `rimz-worktree.json` inside the worktree's Git admin directory (`git rev-parse --git-dir` for that worktree), recording the name, branch, base branch name, base commit, repo root, worktree path, and marker version. Cleanup, `remove`, and `gc` act only when that marker is present; a missing marker reads as user-owned, even if the path matches the configured directory template.

### Seeded files

A new worktree starts ready to run: the project's `.worktreeinclude` lists the untracked files an agent needs — `.env`, local config, caches — as glob patterns, one per line, and Rimz copies each pattern's matches from the checkout into the worktree right after `git worktree add`, preserving the path relative to the repo root. Lines use conventional shell-glob semantics (`*` within a path component, `**` across directories); blank lines and `#` comments are skipped. Matched directories copy recursively.

Seeding stays inside the project root: absolute patterns and patterns reaching out with `..` are skipped, and every file is confined by its canonical path, so a symlink a glob pattern descends into cannot pull host files into the agent-readable worktree. Seeding carries no command execution, so `.worktreeinclude` stays outside the trust hash ([trust.md](../sidebar/trust.md)).

Seeding is best-effort enrichment layered over creation: a missing `.worktreeinclude` is a silent no-op, and a pattern that matches nothing or a file that fails to copy warns on the launch path and is skipped — the worktree and its agent still launch. A reused worktree is never re-seeded. `rimz worktree new` reports the count of seeded files.

## Supervised runs

`rimz agents <spec> <prompt> -p` launches one interactive agent pane, waits for the agent's root turn to end, prints the final assistant message or stream events, and exits with a script-friendly status code: `0` completed, `1` failed, `124` timed out, `130` canceled. It is the headless entry point: a script drives one agent turn and reads the result, with no sidebar and no attached client.

**Run records and completion.** A run record is written under `runs/<run_id>.json` before the pane opens, the launched `rimz agents exec` wrapper exports `RIMZ_RUN_ID`, and lifecycle hooks fold matching root-session observations into that record. The wrapper also records its own normalized pane id when the mux exposes one, so cleanup can close the launched pane without waiting for the sidebar snapshot to bind the agent session. The first root `TurnEnded` completes the run as `completed` or `failed`; a session `Ended` before a turn result marks it failed; `rimz agents stop <run-id>` marks an active run `canceled`; subagent events and same-kind descendant processes with a different session id are ignored, so child completions never finish the parent command. If the wrapper observes the agent process exit and no terminal lifecycle hook lands after a short grace, it writes `failed` and wakes the waiter, making process death a liveness backstop without treating pane exit as success.

Supervised `-p` runs require installed and trusted hooks, because hooks provide the completion signal ([agent.md → Hook install](./agent.md#hook-install--the-visible-security-step)).

**The wakeup socket.** The blocking CLI binds `sock/run.<short_id>.sock` before opening the pane. When a hook, timeout, or operator stop writes the first terminal run record it sends a `run_completed` wakeup frame to that socket; the record on disk remains truth, and the datagram only cuts latency. If the wait cap expires, the CLI reloads the record once to catch a just-written terminal result, otherwise writes `timed_out` and exits `124`.

**Output and input formats.** `--output-format` chooses the projection `-p` prints: `text` (default) prints the final assistant message, `json` prints the terminal run record, and `stream-json` emits NDJSON run events as the turn runs. `--input-format` chooses the prompt source: `text` (default) reads the positional prompt, while `stream-json` reads user messages from stdin until EOF (a bare `content` string or the `text` of each content block), refusing a positional prompt. `stream-json` output is incompatible with `--detach`.

**Streaming is transcript-tail based.** Run records store the adapter transcript path when lifecycle observations provide it; if the first lifecycle hook arrives before the transcript file exists, the first later observation that carries a path writes it once. `rimz agents <spec> <prompt> -p --output-format stream-json` and `rimz agents wait <run-id> --stream` poll that file with the torn-write-safe cursor used for transcript reads, parse only newly appended assistant messages through the selected adapter, and reset the cursor if the transcript path changes. Stream status events come from the same read-time join as `agents show`; the run socket still exists only to wake a blocking producer promptly. An attached `agents wait --stream` timeout stops the watcher and exits `124` without mutating the run record.

**Permission posture is adapter-owned.** The run command chooses `auto`, `ask`, or `yolo`; adapters translate that into provider CLI arguments. Claude maps `auto` to accepted edits and `yolo` to its dangerous bypass flag. Codex maps `auto` to `--ask-for-approval never --sandbox workspace-write`, leaves `ask` at the provider default, and maps `yolo` to its dangerous approvals-and-sandbox bypass.

**Shared launch params are adapter-owned the same way.** `--effort` and `--system-prompt-file` render through each adapter's `render_preset` — the one place per-agent native launch flags are built — so `--effort high` becomes Claude's `--effort high` or Codex's `-c model_reasoning_effort=high`, and `--system-prompt-file` becomes Claude's `--system-prompt-file` or Codex's `-c model_instructions_file=`. An adapter with no native flag for a param refuses the launch, naming the unsupported flag, rather than dropping the intent.

**Durability and inspection.** Run records are cold-path durable state and use temp-file-plus-rename with fsync through the ledger atomic helpers. `rimz agents show <run-id>` reads the current workspace's retained run records and attaches live card context when the run is still active. Live fields stay out of the durable run record, so clearing and agent drift do not create extra locked writes. Records are retained until an operator removes state.

## Cleanup

A worktree and a supervised run are reclaimed when the agent exits, through the same `rimz agents exec` wrapper that launched it.

**Worktree cleanup.** When the agent exits with `--worktree-path`, the wrapper spawns the on-disk `rimz worktree cleanup <path>` helper, resolving past the kernel's trailing ` (deleted)` annotation after an atomic install, so long-lived panes pick up the freshest cleanup logic; if the helper cannot be resolved or spawned, the wrapper falls back to the same cleanup implementation in process.

Cleanup re-reads the marker, checks `git status --porcelain`, checks commits not yet landed on the live base with `git rev-list --count <base>..HEAD`, treats identical base/head trees as landed only when the branch tree differs from its fork point, applies a bounded patch-equivalence check for rebased, cherry-picked, or squash-merged work, and asks the mux for live pane cwd values. If the live base branch is unavailable, cleanup tries `main`, `master`, `origin/HEAD`, then the creation snapshot; if the unmerged count cannot be computed, cleanup treats the worktree as not clean and keeps it.

The cleanup decision is pure:

| Marker | Status | Other live user pane inside path | Decision |
| --- | --- | --- | --- |
| absent | any | any | skip |
| present | clean with no unmerged commits | no | remove worktree and delete the branch after proving its work landed |
| present | dirty or carrying unmerged commits | no | prompt `keep / remove / shell` on a TTY; keep on EOF or non-TTY |
| present | any | yes | skip |

The automatic path deletes a branch only after proving its work landed on the live base: it tries `git branch -d`, escalates to `git branch -D` only after the same landed-work check succeeds, and keeps the branch otherwise. The interactive dirty `remove` choice and `rimz worktree remove --force` use Git's force removal path because the human explicitly chose destruction. Rimz sidebar panes are chrome: they inherit the tab cwd for launch, and worktree liveness reads user panes only.

**Run pane cleanup.** After a blocking `-p` run finishes, pane cleanup is best-effort: Rimz closes the recorded launch pane when available, then falls back to finding the agent row by `(kind, agent_id)` in the snapshot. A detached run (`--detach`) passes cleanup ownership to the in-pane wrapper: unless `--keep` was set, the wrapper watches the run record, terminates the agent process once the run is terminal, performs marked-worktree cleanup, and closes its own pane. Operator stops use the same terminal record and wakeup path, then check whether the recorded pane remains after a short grace and close it through the mux backend when possible; `--keep` controls natural-completion cleanup, not an explicit stop.

**`rimz gc`.** `rimz gc` sweeps clean, marked worktrees whose work has landed on their base in the current repo when no live user pane cwd sits inside them, then runs `git worktree prune`. `Fresh`-based worktrees compare against `origin/...`, so unfetched merges keep them until a fetch updates the remote-tracking base.

## Agent addresses

Every agent in a room has an address you type like an @-mention in a channel: `@<handle>#<channel>`. The handle names who; the channel names where. Both read from context — `@claude` uses the channel you are in, and `#auth` alone filters a listing to that channel.

The channel is the workspace segment the room already groups by: a worktree branch, else a child repo's directory name, else the directory itself — the grouping the sidebar shows. It matches by branch, path basename, or full path, and defaults to the channel the command runs in; an inline `#<name>` or `--worktree` overrides it. A bare directory workspace has no current channel, so an address there reaches every channel rather than silently narrowing to one. Mux tab names stay display-only — they are mutable and live outside the ledger, so they never form an address.

Handles come in two kinds. A **type handle** names a role to fill and carries enough to launch one:

- `@<kind>` — `@codex`, the agent kind. Matches every agent of that kind in the channel, including those launched under a role.
- `@<alias>` — `@planner`, an `[agents.aliases]` role ([configuration.md](../../reference/configuration.md#agent-aliases-and-layouts)). Matches every agent launched under that role.

An **instance handle** names one running agent and only ever addresses what already exists:

- `@<petname>` — `@swift-otter`, the stable per-agent name.
- `@<kind>-<ordinal>` — `@claude-2`, the nth agent of a kind in the channel.
- a session-id prefix.
- `<mux>:<pane>` — `tmux:%1`, a precise, channel-agnostic pane address.

`@all` is the broadcast handle: every agent in the channel.

The rendered handle is the shortest address that names exactly that agent, and it round-trips through the parser. Rimz renders it role-first — the alias when it is unique in scope, else the kind, else `@<kind>-<n>`, else the petname — so a listing always shows the handle you could type back. A handle appears only when typing it reaches that one agent, so two `planner`s in a channel each render as their kind ordinal — `@claude-1` / `@claude-2` — and every handle you see resolves to exactly one agent. One canonical renderer, the inverse of the parser, is shared by every agent-bearing listing (`agents list`, `agents show`, `queue list`, the channel headers); [target.rs](../../../crates/rimz/src/target.rs) owns it.

An address resolves to zero, one, or many agents, and arity decides the outcome:

| Matches | Outcome |
| --- | --- |
| one | delivered |
| many | an ambiguity error listing the handles to pick one, unless `--all` (or the explicit `@all`) opts into the fan-out |
| zero | a miss that names where the agent runs in another channel, or — with `--create` — launches it |

When a type handle matches several agents, the address resolves to an ambiguity that lists the handles to pick one — `rimz steer @codex` with two codexes stops there. `--all` (or `@all`) opts into the fan-out, which confirms before sending unless `-y` skips it; a blocked agent in a fan-out skips while the rest still send.

`--create` launches a missing agent straight from its address. `rimz steer @planner#feat/x --create -- "draft the API"` opens a `planner` in `#feat/x` — creating the worktree when the channel is new — with the text as its first prompt. Only a type handle creates, because only a kind or a role carries what a launch needs; an instance handle (a petname, ordinal, or session id) names something that must already exist and refuses with the fix. Create-on-miss is the same launch as `rimz agents <kind|alias> --worktree=<channel> "<prompt>"`, reached from the address.

## Steering and queuing live agents

Rimz delivers human-authored text to a live agent now (`rimz steer`) or at its next open delivery point (`rimz queue`). Both ride the same pane-send primitive humans and resolvers share, address agents through the [agent-address grammar](#agent-addresses) above, and take state decisions from the ledger snapshot and the hook lifecycle.

### Targets

`steer` and `queue` require the `@` sigil — a bare selector fails with a `did you mean @…?` hint — so a stray word never broadcasts by accident; a pane id is the one sigil-free exception. They resolve the [agent address](#agent-addresses) against a freshly produced snapshot, so a just-started pane is present.

The two commands address different layers, because they deliver at different times. `steer` reaches **live panes**: a bare `@<kind>` or `@all` also reaches a pane that has not bound a session yet — a lazy-registering agent (Codex) before its first turn ([agent.md](./agent.md#the-instance-lifecycle)) — because the address a paste needs is the pane, which the producer already detects for that pane's idle row. `queue` keys a durable record on a session id, so it addresses **bound sessions**; an address that matches only an unbound pane has no key, so `queue` points it at `steer` to start the session first. A petname, kind ordinal, or session-id prefix names a bound session under either command.

Fan-out and `--create` follow the [address rules](#agent-addresses) above: more than one match needs `--all` (or `@all`) and confirms unless `-y`, and one blocked or paneless agent skips while the rest still send.

### Steer

`rimz steer <target> -- <text>` injects into each resolved pane immediately as a [bracketed paste](#bracketed-paste-submit) and then presses Enter as a discrete keystroke outside the paste, so the agent submits instead of taking a newline into its composer. A pending feed ask attached to a bound agent skips that agent; `--force` records the override and sends anyway. The `agent.steered` event records kind, pane id, force flag, and text length per send, plus the session id when one is bound — an unbound pane records only kind and pane. Message content stays out of the event log.

### Bracketed-paste submit

Both `steer` and queue delivery wrap the text in bracketed-paste markers (`ESC[200~` … `ESC[201~`) through the `MuxBackend::paste_text` primitive, then press Enter as a separate `send_key`. Agent composers run paste-detection heuristics: text and a trailing `\r` coalesced into one PTY read are taken as pasted content, and the `\r` becomes a literal newline rather than a submit. The paste markers make the boundary lexical — the composer leaves paste mode on `ESC[201~`, so the following Enter is unambiguously a keystroke even when every byte arrives in one read. The generic `rimz pane send` stays on the raw type path, since a bare shell would render the markers literally.

### Queue layout

Queued messages live under the workspace state root:

```text
queue/<msg_id>.json
queue/terminal/<msg_id>.json
```

`msg_` ids are UUIDv7, so filename order is FIFO order. Pending scans read only `queue/*.json`; claimed and final records move atomically into `queue/terminal/`. The directory is created lazily, so a workspace with no queued messages costs the hook path one missing-dir stat.

Each record stores the workspace id, agent kind, agent session id, text, Enter flag, delivery gate, status, enqueue/update timestamps, attempt count, last attempt timestamp, last error, and delivered timestamp. Status values are `pending`, `claimed`, `delivered`, `removed`, and `abandoned`.

### Gates

`--on done` opens when the rollup status is `idle` or `success`. `--on any` also opens on `failed`. `running`, `waiting`, and `paused` keep delivery closed. A pending ask attached to the agent keeps delivery closed for every gate, because the next input belongs to that ask.

The queue requires installed and trusted hooks for the target agent. Hooks are the delivery signal; accepting a queue entry for an unwired agent would create durable work with no transition that can release it.

### Delivery

Only unparked root turn ends trigger delivery. `Registered`, subagent stops, compaction events, and parked background turn ends do not check the queue. The lifecycle hook records the event, then spawns a detached `rimz queue deliver --message-id <id>` helper with nulled stdio for the FIFO head.

The helper waits `400ms` by default (`RIMZ_QUEUE_SETTLE_MS` overrides this for tests), reads the pending head, checks a fresh snapshot for the gate, the pending-ask predicate, and the bound pane, then claims the head under the workspace lock immediately before sending. State misses leave the message pending for a later transition. The claim moves the record to `claimed`, outside the pending scan, and increments the attempt count. A successful send moves the record to `delivered`; a send failure records `last_error` and returns it to `pending`, and after five attempts the record becomes `abandoned`. The claim timestamp throttles retries after a send failure. A crash after claim leaves a visible `claimed` record that `queue list` surfaces; it is not auto-redelivered on a later turn end.

Delivery is FIFO per agent, and one message is attempted per unparked root turn end.

### Audit events

Queue writes append `message.queued`, `message.delivered`, `message.removed`, and `message.abandoned` events. Events include message id, kind, agent id, gate, status, text length, Enter flag, attempt count, and reason. They never include message text.

`rimz gc` abandons open messages whose `(kind, agent_id)` no longer appears in the current rollup. This is maintenance, not delivery; normal state misses stay pending.

### Hazards

Queued text can still land while a human has half-typed a draft in the agent pane. Rimz gates on ledger state, not focused-pane state or captured composer contents.

Agent UIs can present dialogs that are not represented as feed asks. Core keeps pane capture out of message delivery; resolvers that need to inspect UI text own capture-before-send.

Multiplexer sends are best-effort. A pane can disappear or reject input after the claim. The queue records the error and retries on future turn-end transitions until the attempt cap.
