# The agent harness

> See [DESIGN.md](../../../DESIGN.md) for the commitments this doc operationalizes. The agent *model* — the rollup, state machine, turn phase, liveness, and adapter boundary — is [agent.md](./agent.md); channels and the message system are [message.md](./message.md); Git worktree backing is [worktree.md](./worktree.md); the user-facing commands are [cli/agents.md](../../reference/cli/agents.md). This doc owns the machinery between them: spawning the fleet, addressing it, the supervised runs automation drives, and the cleanup that reclaims its panes.

One agent in one thread is a conversation; tens of agents across a dozen worktrees is a team. The harness runs that team. It spawns agents into panes, reaches any one by name, drives it live or leaves it a task for when it is free, and reclaims its pane when it exits — the same machinery whether a human, a cron job, a CI gate, or a PR hook is doing the driving.

Everything here rides primitives both backends share: a layout compiles to backend-neutral panes, placement lands on a tab or a split, an address resolves through one parser, and a message rides the one pane-send primitive humans and resolvers already use. [cli/agents.md](../../reference/cli/agents.md) is the command surface — flags, synopses, examples; this doc is what those commands do underneath.

## The model

Spawning the fleet separates three independent choices, so any combination is one command: **agents** choose which tools run, **layout** chooses the shape on screen, and **channel** chooses the cooperation lane they run in. `claude,codex` plus `--channel=design` or `--worktree=feat/x` puts a planner and a reviewer side by side in one channel; the same agents with a different layout or channel is the same three knobs turned differently.

Three words name the parts:

- A **channel** is one cooperation lane where a few members work together, backed by a durable bare name, a [worktree](./worktree.md), an in-place named team as `<dir>/<team>`, or the directory room. The sidebar groups the room by it, and an address narrows to it with `#<channel>`.
- A **member** is an agent, named by a **handle**: `@claude` the kind, `@planner` the profile, `@swift-otter` the one running instance.
- An **address** joins them — `@handle#channel` — and is how every command names who it is reaching.

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

## Read the room

`rimz transcript` is the catch-up surface, and the only part worth noting here is the chat-log build. A channel target (`#channel`, `@all#channel`, or a bare invocation in a worktree) reads Rimz's transcript log directly and projects each entry into timestamp order: human prompts render as `user: @receiver, text`, delivered peer messages render as `@sender: @receiver, text` from the entry's structured `from`, assistant replies render as `@receiver: text`, blocking asks render from the agent, and effective answers render from `you` or the resolver to the agent. A single-agent target filters that same channel log to the focal agent's sent and received lines. Peer-opened turns include the receiving agent's assistant reply because the reply is its own transcript entry. Supervised streaming still reads provider-native transcripts through each adapter's `parse_transcript_messages`; the assistant-only `wait --stream` path filters that parse.

## Spawn the fleet

### The layout IR

`rimz agents <spec>` resolves either a named `[agents.teams]` entry or an inline DSL, and both compile to the same backend-neutral panes. The inline grammar is compact: commas split columns, plus signs tile rows within a column, slashes stack rows within a column on Zellij, and each cell is a built-in `term`, an agent kind, a virtual `<kind>-<mode>` / `<kind>-ping` variant (`claude-auto`, `codex-yolo`), a configured profile, or a configured command ([configuration.md](../../reference/configuration.md#agent-profiles-commands-and-teams)). A named team is an ordered role list that opens as one side-by-side column per role unless it declares its own `layout`, which uses the same grammar and resolves declared role names before falling through to roleless cells. A named team also accepts `<team>.<role>` to launch one declared role with its team identity; under the default placement this uses the same placement as any single agent, so it runs in the current pane and returns to the shell on exit, or opens a tab when no launching pane exists.

```text
claude,codex+term      → Claude left; Codex tiled over a shell right
claude/codex/term      → two agents plus a shell in one Zellij stack; tmux tiles them
vim,htop+zsh           → raw command panes
claude-auto,codex-yolo → agent cells with adapter-owned permission posture
```

Stacks are presentation only: Zellij renders a native stack with one expanded pane, while tmux keeps the same cells as tiled rows because it has no native stack.

The compile target is the seam the whole harness hangs off. Each cell becomes a `LayoutPanes` entry that runs the hidden **`rimz agents exec <kind>`** wrapper — carrying the prompt, worktree path, profile, role, model, effort, and `-- <args>` resolved from the profile and role — or, for a command cell, its raw argv (empty argv reserved for the user's shell). Backends never resolve agent kinds or worktrees: the wrapper does. It runs the agent in the pane, inheriting the pane's TTY, launching through the user's shell-startup path when that shell and `/usr/bin/env` are available and falling back to direct exec otherwise. Because the wrapper stays resident, it is also where the supervised-run and cleanup paths below attach.

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

When a cron job, CI gate, PR hook, or script needs to drive one member and read its result, it uses a supervised run. `rimz agents <spec> <prompt> -p` opens one interactive agent pane (splitting the current tab by default, a new tab only with `--new-tab` or outside a room), waits for the agent's root turn to end, prints the result, and exits with a script-friendly code: `0` completed, `1` failed, `124` timed out, `130` canceled. Automation drives one agent turn without attaching to the room; an in-room caller sees the transient pane beside the current one. Supervised runs require installed and trusted hooks, because hooks are the completion signal ([agent.md → Hook install](./agent.md#hook-install--the-visible-security-step)). Loop tasks ride this path ([loop.md](./loop.md)).

**Run records and completion.** A run record is written under `runs/<run_id>.json` before the pane opens, the launched wrapper exports `RIMZ_RUN_ID`, and lifecycle hooks fold matching root-session observations into it. The wrapper also records its own normalized pane id, so cleanup can close the launched pane without waiting for the snapshot to bind the session. The first root `TurnEnded` completes the run `completed` or `failed`; a session `Ended` before a turn result marks it failed; `rimz agents stop <run-id>` marks an active run `canceled`; subagent events and same-kind descendants with a different session id are ignored, so a child completion never finishes the parent. If the wrapper observes the agent process exit and no terminal hook lands after a short grace, it writes `failed` and wakes the waiter — process death is the liveness backstop, and pane exit is never read as success.

**Launch environment.** The wrapper exports `RIMZ_RTK` from `[harness] rtk`; `cargo xtask` uses it to route recognized cargo subcommands through `rtk` for agent runs.

**The wakeup socket.** The blocking CLI binds `sock/run.<short_id>.sock` before opening the pane. The first terminal run record sends a `run_completed` datagram to that socket; the record on disk stays truth and the datagram only cuts latency. If the wait cap expires, the CLI reloads the record once to catch a just-written terminal result, otherwise writes `timed_out` and exits `124`.

**Output and input formats.** `--output-format` chooses the projection `-p` prints (`text` the final assistant message, `json` the run record, `stream-json` NDJSON run events as the turn runs); `--input-format` chooses the prompt source (`text` the positional prompt plus piped stdin, `stream-json` user messages from stdin until EOF). Streaming is **transcript-tail based**: run records store the adapter transcript path, and `--output-format stream-json` / `agents wait --stream` poll it with the torn-write-safe cursor used for transcript reads, parsing only newly appended assistant messages through the selected adapter and resetting the cursor if the path changes. The run socket still exists only to wake a blocking producer promptly.

**Posture and launch params are adapter-owned.** A run chooses `auto` (default), `--ask`, or `--yolo`, and `--model` / `--effort` / `--system-prompt-file` / `--append-system-prompt-file` render through each adapter's `render_preset` — the one place per-agent native launch flags are built. An adapter with no native flag for a param refuses the launch, naming the unsupported flag, rather than dropping the intent (supervised `--max-turns` renders through a separate per-adapter turn-limit hook). The provider-specific mappings live in the adapter docs.

**Durability and inspection.** Run records are cold-path durable state, written with temp-file-plus-rename through the ledger atomic helpers and retained until an operator removes state. `rimz agents show <run-id>` reads the retained records and attaches live card context while the run is active; live fields stay out of the durable record, so clearing and agent drift create no extra locked writes.

## Cleanup

When an agent exits, the same `rimz agents exec` wrapper that launched it reclaims what it owns: the supervised run's pane, and — for a worktree launch — the worktree.

**End traces.** Interactive agent panes keep the wrapper resident. A clean child exit is deliberate. A signal exit from a tab/pane close is deliberate when the mux session still exists, even if the room is mid-teardown or missing sidebar chrome. The wrapper records a durable `agent.ended` trace before slower cleanup, so that agent stays out of future recovery. When the mux session itself is gone at wrapper exit — reboot, mux crash, closing the last tab, in-`start` stuck-room recovery — the wrapper preserves recovery state so resume birth can regroup the agent into a `#<channel>` tab.

**Worktree cleanup.** An agent launched with `--worktree-path` triggers worktree cleanup on deliberate exit, which proves the branch's work landed before removing the tree and deleting its branch. Clean quits keep the existing interactive helper attached to the pane. Signal exits start the helper with null stdio in its own process group, so cleanup can finish after the closing pane disappears. The helper, decision table, and `rimz gc` sweep are [worktree.md → Cleanup](./worktree.md#cleanup).

**Run-pane cleanup.** After a blocking `-p` run finishes, pane cleanup is best-effort: Rimz closes the recorded launch pane, falling back to finding the agent row by `(kind, agent_id)` in the snapshot. A detached run (`--detach`) passes cleanup to the in-pane wrapper: unless `--keep` was set, the wrapper watches the run record, terminates the agent once the run is terminal, performs marked-worktree cleanup, and closes its own pane. An operator stop uses the same terminal record and wakeup path, then closes the recorded pane if it lingers past a short grace — reclaiming a kept run's pane whether the ref is the run id or the agent name.
