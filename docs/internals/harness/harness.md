# The agent harness

> See [DESIGN.md](../../../DESIGN.md) for the commitments this doc operationalizes. The agent *model* — the rollup, state machine, turn phase, liveness, and adapter boundary — is [model.md](../agents/model.md); the message system and its channel lanes are [messaging.md](./messaging.md); Git worktree backing is [worktrees.md](./worktrees.md); the user-facing commands are [cli/agents.md](../../reference/cli/agents.md). This doc owns the machinery between them: spawning the fleet, addressing it, the supervised runs automation drives, the scheduled loop tasks that drive those runs on a clock, and the cleanup that reclaims panes and worktrees. Two more `harness/` subsystems are documented where their behaviour surfaces: auto-continue, the opt-in nudge that resumes rate-limited turns ([provider.md § Auto-continue](../agents/providers.md#auto-continue)), and resume planning, which reseeds a reboot- or crash-killed fleet at the next birth and relaunches an explicit cohort in a live room ([sidebar.md § Resume-on-rebirth](../sidebar/sidebar.md#resume-on-rebirth)).

One agent in one thread is a conversation; tens of agents across a dozen worktrees is a team. The harness runs that team. It spawns agents into panes, reaches any one by name, drives it live or leaves it a task for when it is free, and leaves the pane usable or reclaims it when the agent exits — the same machinery whether a human, a cron job, a CI gate, or a PR hook is doing the driving.

Everything here rides primitives both backends share: a layout compiles to backend-neutral panes, placement lands on a tab or a split, an address resolves through one parser, and a message rides the one pane-send primitive humans and scripts already use. [cli/agents.md](../../reference/cli/agents.md) is the command surface (flags, synopses, examples); this doc is what those commands do underneath.

## The model

Spawning the fleet separates three independent choices, so any combination is one command: **agents** choose which tools run, **layout** chooses the shape on screen, and **channel** chooses the cooperation lane they run in. `claude,codex` plus `--channel=design` or `--worktree=feat/x` puts a planner and a reviewer side by side in one channel; the same agents with a different layout or channel is the same three knobs turned differently.

Three words name the parts:

- A **channel** is one cooperation lane where a few members work together, backed by a durable bare name, a [worktree](./worktrees.md), an in-place named team as `<dir>/<team>`, or the directory room. The sidebar groups the room by it, and an address narrows to it with `#<channel>`.
- A **member** is an agent, named by a **handle**: `@claude` the kind, `@planner` the profile, `@writer` the user-chosen instance name, `@swift-otter` the minted instance petname.
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

`rimz agents <spec>` resolves either a named `[agents.teams]` entry or an inline DSL, and both compile to the same backend-neutral panes. The inline grammar is compact: commas split columns, plus signs tile rows within a column, slashes stack rows within a column on Zellij, and each cell is a built-in `term`, an agent kind, a virtual `<kind>-<mode>` / `<kind>-ping` variant (`claude-auto`, `codex-yolo`), a configured profile, or a configured command ([configuration.md](../../guide/configuration.md#agent-profiles-commands-and-teams)). An agent cell may carry an ad-hoc role as `<cell>:<role>`; inline roles follow team-role name and address rules, stay unique within the spec, and apply only to agent cells. A named team is an ordered role list that opens as one side-by-side column per role unless it declares its own `layout`, which uses the row and column grammar and resolves declared role names before falling through to roleless cells; team layout strings keep roles in the team's declared role list and do not accept the inline suffix. A named team also accepts `<team>.<role>` to launch one declared role with its team identity; that single role places like any single-agent launch ([placement](#backend-shape-and-placement)).

```text
claude,codex+term      → Claude left; Codex tiled over a shell right
claude/codex/term      → two agents plus a shell in one Zellij stack; tmux tiles them
vim,htop+zsh           → raw command panes
claude-auto,codex-yolo → agent cells with adapter-owned permission posture
claude:planner,codex:coder → agent cells with ad-hoc `@planner` and `@coder` handles
```

Stacks are presentation only: Zellij renders a native stack with one expanded pane, while tmux keeps the same cells as tiled rows because it has no native stack.

The compile target is the seam the whole harness hangs off. Each cell becomes a `LayoutPanes` entry: an agent cell compiles to the exec-wrapper argv (below), a command cell to its raw argv, and an empty argv reserves the pane for the user's shell. A trailing launch prompt is attached to one agent identity and wrapper argv: a named team's configured `leader` role, its first declared role by default, or otherwise the first unambiguous agent cell. Team and multi-cell launches stamp each member's cohort and order (`launch_group`/`launch_ordinal`, exported as `RIMZ_LAUNCH_GROUP`/`RIMZ_LAUNCH_ORDINAL`) so the sidebar keeps the cards in definition order.

`harness::plan::finalize_launch_layout` is the launch-finalization seam: it applies permission posture, CLI presets and passthrough argv, budget, adapter-declared preset reconciliation and defaults, and supervised turn limits to the resolved `LayoutSpec` before pane compilation.

### The exec wrapper

Every agent pane runs the hidden **`rimz agents exec <kind>`** wrapper rather than the agent directly. The wrapper argv carries everything the launch resolved: the prompt, worktree path, profile, role, model, effort, and `-- <args>` from the profile and role. Backends never resolve agent kinds or worktrees; the wrapper does.

The wrapper runs the agent in the pane, inheriting the pane's TTY. It launches through the user's shell-startup path when that shell and `/usr/bin/env` are available, falling back to direct exec otherwise, and it exports `RIMZ_RTK` from `[harness] rtk` into the run, which `cargo xtask` reads to route recognized cargo subcommands through `rtk`.

Room birth also carries one generic adapter-enrichment environment map through the mux seam. RimZ-managed launches still apply their adapter `launch_env` last, while a stock agent typed directly into the ordinary work shell inherits the room baseline. Existing processes and shells cannot be upgraded retroactively; rebirth is the parity boundary on both backends.

The wrapper stays resident behind the agent whenever it has work left after the agent exits (a run to complete, an idle shell to leave in a close-pane or worktree pane, a worktree to reclaim, a pane to close), which makes it the attach point for supervised runs ([below](#supervised-runs)) and for the end traces and reclamation in [Cleanup](#cleanup). A plain in-place launch has none of those, so the wrapper direct-execs the agent and the pane returns straight to the shell.

### Backend shape and placement

Each backend renders the same compiled layout into a tab, and a single non-worktree cell can run in the current pane instead. Both backends receive the same `TabOptions` — session, title, cwd, focus flag, sidebar options, and the pre-built pane argv — and dock the global sidebar once before adding the layout cells; the per-backend split commands live in [`mux/`](../../../crates/rimz/src/mux/AGENTS.md). A named-channel or worktree launch names its tab `#<NAME>`, matching the channel suffix in agent addresses; a named team launch names it `team:<name>` and stamps its in-place lane as `<dir>/<team>`; any other non-worktree launch names it `<kind>:<dir>`. `--bg` keeps focus on the launching pane wherever the backend can.

**Placement resolves before the launch touches the store or creates a worktree**, so a rejected placement leaves no provisional rows or worktree behind. Under the `auto` default a single non-worktree cell launches *in the current pane*: the CLI execs the wrapper argv in place, the wrapper binds the pane and direct-execs the agent, and the pane returns to its shell on exit with liveness resolved from the pane rather than an end trace. A named-channel launch, multi-cell layout, or worktree launch opens its own tab. `--new-pane` splits the current tab, `--new-tab` opens a tab, and the per-machine [`[agents] placement`](../../guide/configuration.md#agent-profiles-commands-and-teams) default chooses when no flag is given. `--bg` and create-on-miss downgrade an in-place launch to a split, because the caller's pane stays available; the split carries the launch-identity env on both backends and honors the same focus flag.

Cohort relaunch reconciliation runs after the live-room preflight and before worktree resolution when the command names a team or an inline layout with at least two agent cells and supplies an explicit `-w NAME`. It derives the named worktree path without creating it, reads the audit rollup for matching root members in that path, and chooses one of four outcomes: no history continues into the ordinary launch path, live history focuses the newest bound member and exits, closed history with dirty or unproven work offers a worktree-scoped resume, and closed history whose status is clean and content-landed offers to remove the worktree before continuing into a fresh launch. Named-team reconciliation considers every member of that team in the target worktree, including sibling roles when a single-role spec relaunches; inline membership matches by launch group, then ordinal, then kind and role, with a final kind-only fallback for legacy role-less records.

### Cohort resume

Resume planning has three admits. Room rebirth uses the persisted live roster intersected with the audit rollup to seed one tab per live-at-death stamped lane or worktree before the new mux session starts. A named team restores in its declared layout, resuming members that can resume and fresh-launching missing or unsupported agent cells so the shape stays whole; non-team lanes restore as one column. `rimz agents <spec> --resume` uses the same rollup in a live room to bring back a prior cohort after a tab or pane was closed; `--continue` is the same visible alias; `-w <NAME>` or the caller's current worktree scopes it to one exact worktree, and a project-root resume keeps the newest-by-spec behavior. `rimz agents resume <scope>` selects by lane instead: it reuses the rebirth team/flat split when every member is closed, sends flat resume commands beside a surviving live member for a partial lane, and focuses the freshest pane when every member is live.

A named team spec matches prior root agents with the same `team` and then maps role cells by role, taking the newest member per role. An inline multi-agent spec matches the newest `launch_group` that maps onto the agent cells by `launch_ordinal`, falling back to kind when old records lack ordinals. A single-agent spec ignores cohort membership and resumes the newest dead or unknown root session of that kind. Missing cells launch fresh in the matched cohort's cwd and channel, so the layout stays whole.

Cohort resume keeps cleanly ended members as candidates, so a closed team resumes when its worktree still exists; reboot team restore uses the same cohort planner. Subagents, empty session ids, and missing worktrees are not resume candidates. A matched member whose process is still live refuses the whole resume with the live member named, because launching beside it would duplicate the addressable role or kind. A kind whose adapter has no native resume argv launches fresh and is reported as such.

Resume panes run `rimz agents exec <kind> --resume <session-id>` with the prior RimZ identity flags (`--agent-name`, profile, role, team, launch group, launch ordinal, and channel). The exec resume grammar accepts trailing provider arguments for an explicit in-room restart, while resume-on-rebirth passes none to keep its argv stable; the launch event records the resolved permission mode so restart can reproduce that posture. Cohort resume does not replay prompt, model, effort, system-prompt, or passthrough flags; `--resume` and `--continue` conflict with those launch-shaping flags and take cwd/channel from the matched store cohort. `--worktree` is a resume scope in this mode rather than a worktree-creation flag.

## The address

Every member has an address you type like an @-mention: `@<handle>#<channel>`. The handle names who, the channel names where, and both read from context — `@claude` uses the channel you are in, `#auth` alone filters a listing to that channel. [cli/agents.md → Addressing agents](../../reference/cli/agents.md#addressing-agents) is the handle catalog; this section owns how an address resolves.

The **channel** is the workspace segment the room already groups by: an explicit named channel, else a worktree name, else an in-place team stamped at launch as `<dir>/<team>`, else a directory basename fallback for unstamped agents ([message.md § Channels](./messaging.md#channels)). It matches by exact stamped lane, path basename, or full path, and defaults to the channel the command runs in; an inline `#<name>`, `--channel`, or `--worktree` overrides it. A bare directory workspace has no current channel for humans, so an address there reaches *every* channel rather than silently narrowing to one; RimZ-launched panes carry `RIMZ_CHANNEL`, while `RIMZ_TEAM` remains cohort identity for team members. Branch names stay display metadata. Mux tab names stay display-only — they are mutable and live outside the store, so they never form an address.

A **handle** falls into three classes, narrowing from group to instance:

- A **role handle** (`@coder`) names a team role and matches every agent launched under it in the channel. Role names reserve built-in kind handles so kind addresses keep round-tripping.
- A **type handle** names a kind (`@codex`) or a profile (`@planner`) and matches every agent of it in the channel. It carries enough to launch one, so only a type handle can create.
- An **instance handle** names one running agent and only ever addresses what exists: an explicit launch name (`@writer` from `--name writer`), a petname (`@swift-otter`), a kind ordinal (`@claude-2`), a session-id prefix, or a precise `<mux>:<pane>` pane address. `@all` is the broadcast handle for the whole channel.

The petname is the harness's stable per-instance fallback name: the store mints an adjective-noun pair at registration, collision-checked against the room's live names and refusing reserved command words and kind-shaped names, so a petname can never shadow `@all` or `@claude-2`. User-chosen names are explicit launch identity and render first after a role; minted and worktree-derived soft names stay fallback instance selectors. A session recorded before petnames re-derives one deterministically from its session id, so old logs still render a stable name. [petname.rs](../../../crates/rimz/src/harness/petname.rs) owns the generator.

The rendered handle is the shortest address that names exactly that agent, and it round-trips through the parser. RimZ renders it role-first — the role when unique in scope, then the explicit `--name`, then the profile when unique, else the kind, else `@<kind>-<n>`, else the petname — so a listing always shows a handle you could type back, and a handle appears only when typing it reaches that one agent. One canonical renderer, the inverse of the parser, is shared by every agent-bearing listing; [target.rs](../../../crates/rimz/src/harness/target.rs) owns both.

An address resolves to zero, one, or many agents against a fresh snapshot, and arity decides the outcome:

| Matches | Outcome |
| --- | --- |
| one | delivered |
| many | an ambiguity error listing the handles to pick one, unless `--all` or `@all` opts into fan-out; fan-out delivers to every match, prefixes each delivery with the addressed handle (`@all,`, `@claude,`) so receivers read it as a group message, and skips a blocked agent while the rest send |
| zero | a miss that names where the agent runs in another channel and lists live agents, or — with `--create` — launches it |

`--create` launches a missing agent straight from its address: `rimz message --steer @planner#design --create "draft the API"` opens a `planner` in `#design`, registering the named channel, with the text as its first prompt. With `--worktree feat/x`, create-on-miss creates or reuses the worktree instead. Only a type handle creates, because only a kind or profile carries what a launch needs; an instance handle names something that must already exist and refuses with the fix.

## Talk and queue

The message system — send modes, the durable message record, delivery gates and FIFO ordering, the hook-triggered delivery pipeline, scheduling, smart compaction, wait confirmation, retries, and the audit trail — lives in [messaging.md](./messaging.md). The user-facing command surface is in [cli/message.md](../../reference/cli/message.md).

## Supervised runs

When a cron job, CI gate, PR hook, or script needs to drive one member and read its result, it uses a supervised run. `rimz agents <spec> <prompt> -p` opens one interactive agent pane (splitting the current tab by default, a new tab only with `--new-tab` or outside a room), waits for the agent's root turn to end, prints the result, and exits with a script-friendly code: `0` completed, `1` failed, `123` verify failed, `124` timed out, `125` budget exceeded, `130` canceled. Run records carry the launch budget, terminal observed cost, fresh input/output token totals, and optional verify evidence, so loop history can enforce a daily fire gate and render per-fire spend without rescanning transcripts. Automation drives one agent turn without attaching to the room; an in-room caller sees the transient pane beside the current one. Scheduled loop runs add one placement hint: they target the locked `rimzd` loop panel, whose held watcher ignores quit keys, and stack in that loop zone; while the view survives, elder-tick repair restores any closed managed pane and fire-time repair restores a missing loop panel immediately, while a missing view or failed split falls back to a new tab. Supervised runs require installed and trusted hooks, because hooks are the completion signal ([agent.md → Hook install](../agents/model.md#hook-install-the-visible-security-step)). RimZ's built-in scheduler drives this same path on a clock ([Scheduled turns](#scheduled-turns-loop)).

**Run records and completion.** A run record is written under `runs/<run_id>.json` before the pane opens, the launched wrapper exports `RIMZ_RUN_ID`, and lifecycle hooks fold matching root-session observations into it. The wrapper also records its own normalized pane id, so cleanup can close the launched pane without waiting for the snapshot to bind the session. The first root `TurnEnded` completes the run `completed` or `failed`; a session `Ended` before a turn result marks it failed; `rimz agents stop <run-id>` or Ctrl+C on the blocking caller marks an active run `canceled`; subagent events and same-kind descendants with a different session id are ignored, so a child completion never finishes the parent. If the wrapper observes the agent process exit and no terminal hook lands after a short grace, it captures its own pane tail, writes `failed`, and wakes the waiter — process death is the liveness backstop, and pane exit is never read as success. A retried run writes one record per attempt, points each retry's `retry_of` at the preceding attempt, and stores the augmented prompt that attempt received; retry launch names are soft so an explicit name is reused after the prior card leaves or reminted when pane-exit tombstoning is still in flight.

**The run wake.** The blocking CLI binds `sock/run.<short_id>.sock` before opening the pane ([`run_wake.rs`](../../../crates/rimz/src/harness/run_wake.rs)). The first terminal run record sends a `run_completed` datagram to that socket; the record on disk stays truth and the datagram only cuts latency. The waiter validates every frame by `(workspace_id, run_id)` and drops a mismatch, and re-checks the durable record every tick, so a mismatched or lost datagram costs at most one tick. If the wait cap expires, the CLI reloads the record once to catch a just-written terminal result, otherwise writes `timed_out` and exits `124`.

**Verification re-arms the same run.** After a completed turn, `--verify` runs its shell command in the run cwd. A red result writes the evidence, reopens the record from `completed` to `running` before delivery, and sends the evidence prompt through the durable message path into the same pane and agent session. The next root `TurnEnded` can therefore make the record terminal again and send another datagram to the same bound wake socket; no provider resume or replacement pane enters the path. Passing leaves the run `completed`, while exhausting the total turn cap records `verify_failed`.

**Output and input formats.** `--output-format` chooses the projection `-p` prints (`text` the final assistant message, `json` the run record, `stream-json` NDJSON run events as the turn runs); `--input-format` chooses the prompt source (`text` the positional prompt plus explicit `--stdin` content in `<stdin>` tags when both are present, `stream-json` user messages from stdin until EOF). Text output keeps stdout as the answer channel; failed, timed-out, or canceled runs print a stderr forensics block with status, captured pane tail when present, and transcript path. Streaming is **transcript-tail based**: run records store the path to the agent's own provider-native transcript, not the RimZ transcript log that `rimz transcript` renders. `--output-format stream-json` and `agents wait --stream --json` emit NDJSON run events; plain `agents wait --stream` renders assistant text. Both wait streams poll the provider transcript with the torn-write-safe cursor used for transcript reads, parse only newly appended assistant messages through the selected adapter's `parse_transcript_messages`, and reset the cursor if the path changes. The run socket still exists only to wake a blocking producer promptly.

**Posture and launch params are adapter-owned.** A run chooses `auto` (default), `--ask`, or `--yolo`, and `--model` / `--effort` / `--system-prompt-file` / `--append-system-prompt-file` render through each adapter's `render_preset` — the one place per-agent native launch flags are built. After profile and CLI arguments merge, launch reconciles adapter-declared preset flags against raw `args`, adopts an args-only model as identity, and then stamps an adapter default only when no model was selected. An adapter with no native flag for a param refuses the launch, naming the unsupported flag, rather than dropping the intent (supervised `--max-turns` renders through a separate per-adapter turn-limit hook). The provider-specific mappings live in the adapter docs.

**Durability and inspection.** Run records are cold-path durable state, written with temp-file-plus-rename through the store atomic helpers and retained until an operator removes state. A failed, timed-out, or canceled blocking run captures the transient pane before cleanup when no earlier wrapper capture exists; first failure-tail writer wins, so a died-early wrapper capture cannot be overwritten by later cleanup. `rimz agents show <run-id>` reads the retained records and attaches live card context while the run is active; live fields stay out of the durable record, so clearing and agent drift create no extra locked writes.

## Dollar budget scopes

The budget engine evaluates agent, room-fleet, and provider-account caps on every producer tick. Agent caps keep their per-session `budget.<digest>.json` ledger; the room writes `budget.fleet.json`; each provider login writes machine-shared `budget.account.<kind>.json`; and `budget.scopes.json` carries per-agent fleet/account waivers, park thresholds, and interrupt throttles. These are cache-class atomic files resolved from launch config, machine config, runtime overrides, and transcript-derived spend; scope-ledger locks let producer ticks merge only park state without clobbering a concurrent CLI cap change.

Room and account caps use the spending walk's dedicated local-day windows rather than session baselines: the workspace cache excludes live sessions and the fold adds their current card costs back, while the shared provider cache publishes a walked per-kind day tally. A scope at or over cap stamps its park, interrupts a running pane through the hidden `agents budget-park` helper, and arms auto-continue for the next local day. Agent park display wins over fleet, which wins over account.

A delivered human message after a park stamps one waiver for that agent across the fleet and account scopes. A running turn started after the stamp proceeds; its terminal transition consumes the waiver and restores the park. Resume-gated and background messages do not waive. The supervised-run and loop-fire entry gates read the same effective ledgers and local-day caches without accepting a waiver; `-p` exits `125`, while loops append `budget skipped`.

## Scheduled turns (loop)

`rimz loop` runs a turn on a clock. The room's elected sidebar elder — the producer node ([state.md → The node model](../sidebar/state.md#the-node-model)) — keeps time while a room for the task's project is open, and on its data tick fires `rimz loop run <name>` for every task that has come due. A task drives one of three actions: `agent` spawns one transient supervised pane down the [supervised-run](#supervised-runs) path, `wake` delivers a prompt to one live agent through the [message](./messaging.md) path, and `check` runs a shell command that either stands alone or guards one of the other two. Everything below is what the elder and the hidden `rimz loop run` do underneath; the command surface — flags, synopses, examples — is [cli/loop.md](../../reference/cli/loop.md).

An `agent` task names exactly one agent cell: a built-in kind, a profile, or an adapter-supported virtual cell such as `claude-auto`, `codex-yolo`, or `claude-ping`. Teams, multi-cell layouts, and command cells are rejected at add time, because a scheduled task owns one supervised pane.

### Schedule forms

`rimz loop add` validates the task, runs hook preflight when it carries an agent action, and makes it live immediately while a room for its project is open. Durable recurring definitions live in per-machine `loop.toml`; `rimz loop add --project` writes shared trusted definitions to `<root>/.rimz/config.toml`; RimZ-generated ephemerals live in state (below). A task carries one firing shape:

- **One-shot** — bare `at = "07:00"` or `rimz loop add --in 30m`; the task removes itself after one scheduled fire.
- **Interval** — `every = "15m"`, `2h`, or `1d`. The elder fires at the exact interval measured from the last arm or fire.
- **Calendar** — `every = "weekday"` plus `at = "07:00"`, where the day mask is `day`, `weekday`, `weekend`, a range like `mon-fri`, or a list `mon,wed,fri`. Wall-clock evaluation uses the configured `timezone`, falling back to the system zone when unset.
- **Raw cron** — `cron = "*/15 * * * *"`, matched by an in-process five-field matcher over minute, hour, day-of-month, month, and day-of-week in the configured `timezone`.
- **Window-reset** — `every = "reset"` on a `<kind>-ping` agent task. The elder fires from the provider's longest cached budget-window reset stamp plus one minute.
- **Poll-until** — `every = "2m"` with `check`, `on`, an agent action, and `deadline`. `rimz loop add --until 30m` stores the resolved absolute deadline in instance state.

An ephemeral task — a one-shot, or any task with a `deadline` — removes its own state row before the supervised run or delivery. A one-shot removed pre-fire that then fails to launch is not retried. A poll-until task also removes itself when its check fires the agent action, and expires without delivery once its `deadline` passes.

### Elder firing

The elder keeps a per-room `loop-fire.json` map of task name to last-fire `Timestamp` under the workspace runtime dir. First sight arms a task by recording `now` and does not fire; the next matching occurrence fires. A fire records `now` before spawning the detached helper, so a hot sub-interval tick cannot spawn the same occurrence twice.

Machine-local `loop-pauses.json` overlays task names from every definition store, and `loop-strikes.json` stores each task's consecutive failure count independently of run-log rotation. An active pause holds an existing arm/fire stamp unchanged; a task first seen while paused still arms without firing. The runner classifies and records a strike signal with every run record, auto-pauses at the configured threshold, and fires `loop_paused` notifications. When a timed pause expires or `loop resume` stamps its end and clears its strikes, that end becomes the effective last-fire edge, so each schedule waits for its next occurrence instead of replaying fires missed during the pause.

Arming stamps the first-sight time, and that stamp sets the firing edge each schedule reads. A calendar task fires at the first tick at or after its wall-clock time on a matching day, at most once that day — so a tick a few seconds late still fires it, but a task first seen *after* its time today waits for the next matching day. A cron task fires only on a tick whose minute matches, so a room opened past a matching minute waits for the next match. An interval task fires once the measured elapsed time crosses the interval.

Each room fires only tasks whose normalized `root` maps to its `WorkspaceId`. `rimz loop add` writes a canonical absolute root for machine tasks; a hand-edited `~` or relative root is expanded and canonicalized before the ownership check, display, and execution. Project tasks inject the project root at load time, reject `root`, `wake`, and `deadline`, and require `every` or `cron` because a committed task cannot choose another workspace, pin a local session, carry a poll-until timestamp from one machine, or delete itself from the trust-hashed file on fire.

The elder unconditionally spawns `rimz loop run <name>` with fresh null stdio for each due fire. The runner applies project trust, daily-budget, room and provider-account scope-budget, and surplus gates in both scheduled and manual modes; budget and surplus gates record their skip result. It then resolves the recorded root, takes a per-task advisory lock in the room runtime dir, runs any `check` first, applies agent hook preflight only when the guard fires, and launches the supervised pane or messages the pinned session. Once a task is loaded, the runner appends exactly one history record: mode, duration, terminal result, cost, fresh input/output token totals, check exit/timeout/output tail, error chain, delivery target, and supervised run id/last message when present.

The lock file is `loop-run-<name>.lock` next to `loop-fire.json`, carries the holder's `{pid, started_at}`, and the kernel releases it when the runner exits or crashes; display probes inspect the lock without rewriting its payload. A due fire or manual `loop fire` that meets a still-running task records `overlapped`, reports the holder and its age when metadata is available, and leaves task state untouched.

`rimz loop fire <name>` drives the same runner path in the foreground for testing, streams check output live, prints the outcome, the agent's final message, and failure evidence, and leaves one-shot entries and wake schedules in place. `--keep` leaves the transient supervised pane open for inspection; scheduled `loop run` captures check output for run forensics and always lets the run cleanup reclaim panes.

Self-paced loops are ordinary one-shots. An agent schedules its next wake with `--in <delay>` at the end of the current wake; the instance row is removed before delivery, so the agent creates the next one only while it still has work. The pending wake stays visible in `rimz loop list` without editing `loop.toml`.

### Script checks

`check = "<shell>"` runs through `sh -c` at the task's project root before any agent action. `on = "fail"` — the default — wakes on a non-zero exit or a timeout; `on = "success"` wakes on a zero exit. `timeout = "5m"` bounds the check, falling back to five minutes when unset.

A check-only task is a scheduled command with no agent action: it logs `completed`, `failed`, or `timed out` with the exit code and capped combined output, and keeps recurring unless it is ephemeral. A guarded task logs the check evidence whether it skips or fires; when the guard fires, RimZ appends the command, its exit status, and the capped combined output to the base prompt before spawning or delivering.

Two patterns fall out of the guard. The watchdog runs a command on a schedule and wakes an agent on failure (`every = "15m"`, `check = "cargo test"`, `on = "fail"`, `agent = "codex"`). The trigger-when-green polls until a command succeeds, then delivers (`every = "2m"`, `check = "gh run watch --exit-status"`, `on = "success"`, `wake = ...`, `deadline = ...`). A poll-until instance stops in one of two cases: the first matching check result fires the agent action, or the `deadline` passes and the run logs `expired`.

Script checks in per-machine `loop.toml` are personal automation, like a crontab. Script checks in project `[tasks]` are shared automation: they are part of `.rimz/config.toml`, enter the project trust hash, show as `project · untrusted` or `project · stale` before grant, and are skipped by the elder until trusted. `rimz loop run` refuses an untrusted project-only task with the trust-grant fix, while a same-named machine task keeps running during the untrusted window.

### Delivering to a live instance

`wake` pins a schedule to one exact agent session. `rimz loop add <name> --wake @<handle> ...` resolves the address against the live rollup at add time, records a `wake` sub-table of `kind`, `session`, and `handle`, and rejects `agent` and supervised-run flags because delivery opens no pane.

On fire, the runner resolves the recorded `root`, confirms the pinned root session still exists, and sends the prompt through the same [message](./messaging.md) path as `rimz message`, gated `done`. An idle agent receives it immediately; a running agent parks it for its next `done` turn boundary; a missing session is skipped and the schedule removed, because that exact conversation cannot return. `rimz gc` runs the same liveness check and reaps wake schedules whose pinned session has left the rollup — a safety sweep for tasks that never fired successfully after the agent exited.

### Window-priming pings

An `agent` value ending in `<kind>-ping` starts a provider's budget window at a time you choose. It runs at the lowest effort unless configured otherwise, while the virtual cell supplies the adapter's effort flags, and Claude's ping pins Sonnet so a large-context account does not prime at the flagship rate. Declare the task prompt explicitly, usually `prompt = "ping"`. The window is account-scoped, shared by every session of a provider kind ([provider.md → Window fusion](../agents/providers.md#window-fusion)), so one ping per provider primes the whole account. Ping turns count in spend totals, but the session spend-window detector treats them as loop-fired automation rather than human activity. Before spawning the turn, the runner reads the shared rate-limit cache and skips when the shortest window is already counting down. The read is best-effort: an unknown or cold cache falls through to the ping, since missing a window-start defeats the feature while an occasional extra token is cheap.

`every = "reset"` lets a ping follow the provider's longest observed budget window. The occurrence is the raw cached `resets_at` for the longest dated window plus one minute, so a passed reset remains the edge until a real provider reading refreshes the cache. The runner gate checks the longest window for this shape; if that window is already counting down, the ping records `skipped window`. The ping turn's own status or account reading stamps the next reset occurrence. A never-started or cold-cache window schedules nothing until organic use or a manual fire creates a cached reset, while a room reopened after the reset catches up from the stale raw stamp.

### State and code

Durable definitions live in `~/.config/rimz/loop.toml` and trusted `<root>/.rimz/config.toml` under `[tasks.*]`. `schedule/catalog.rs` reads visible and runnable precedence together: visible project definitions shadow machine and instance rows regardless of trust, while runnable precedence admits only trusted project rows and falls back to a same-named machine task. Machine-generated one-shots, self-wakes, and poll-until instances live in `~/.local/state/rimz/loop-instances.json` with the same task shape; the catalog coordinates source mutation, scheduled consumption, and pause/strike overlays. Per-room arm/fire stamps live in runtime `loop-fire.json`; per-task run locks live beside it. `Schedule::next_after` combines the effective last-fire stamp with the configured timezone so `rimz loop list` and `rimz loop show` render the NEXT column as `due`, `in 12m`, `paused`, or `-`. User-global run history lives in state `loop-runs.log.jsonl`, and `rimz loop show <name>` reads it for recent runs plus the newest stored check output, error chain, delivery target, run id, last message, pane output tail, and transcript path when the run store still has it.

- [`schedule.rs`](../../../crates/rimz/src/harness/schedule.rs) — typed task actions plus parsing, descriptions, due evaluation, and next-occurrence calculation.
- [`schedule/catalog.rs`](../../../crates/rimz/src/harness/schedule/catalog.rs) — visible/runnable precedence, source-aware mutation, scheduled consumption, and maintenance.
- [`schedule/runner.rs`](../../../crates/rimz/src/harness/schedule/runner.rs) — runnable compilation, check execution, prompt resolution, per-task run locks, and window gates.
- [`cli/loop_cmd/`](../../../crates/rimz/src/cli/loop_cmd) — argument translation, terminal orchestration, and rendering.
- [`schedule/config_edit.rs`](../../../crates/rimz/src/harness/schedule/config_edit.rs) — comment-preserving machine and project task-store editing.
- [`schedule/instances.rs`](../../../crates/rimz/src/harness/schedule/instances.rs) — private ephemeral instance storage used by the catalog.
- [`schedule/pauses.rs`](../../../crates/rimz/src/harness/schedule/pauses.rs) — the machine-local pause overlay and effective-last-fire rule.
- [`schedule/fire.rs`](../../../crates/rimz/src/harness/schedule/fire.rs) — elder firing and the `loop-fire.json` state.
- [`schedule/run_log.rs`](../../../crates/rimz/src/harness/schedule/run_log.rs) — terminal outcome conversion, history, and strike-to-pause transitions.

## Cleanup

When an agent exits, the same resident [exec wrapper](#the-exec-wrapper) that launched it leaves the pane usable or reclaims what automation owns: the supervised run's pane, and — for a worktree launch — the worktree. On a clean interactive exit from a close-pane or worktree pane, the wrapper records the end trace, prints one exit and relaunch hint, and execs the user's shell in that pane. Supervised runs and abrupt rimz-driven teardown reclaim directly.

**End traces and abrupt exits.** Interactive agent panes keep the wrapper resident. A clean child exit is deliberate. An abrupt exit from a tab/pane close is deliberate when the mux session still accepts live pane closes, even if the room is mid-teardown or missing sidebar chrome. The wrapper records a durable `rimz.agent-ended` trace before slower cleanup, so that agent stays out of future recovery. When the wrapper exits abruptly and the mux session is gone, wedged, or resurrected — reboot, mux crash, closing the last tab, in-`start` stuck-room recovery — it skips worktree cleanup; recovery state comes from the sidebar producer's latest live roster. The probe is stronger than a bare session listing, so a wedged-but-listed Zellij server still counts as abrupt while a live room with missing sidebar chrome still treats a pane close as deliberate.

**Worktree cleanup.** An agent launched with `--worktree-path` triggers worktree cleanup on supervised-run completion or signal/tab-close exit, which proves the branch's work landed before removing the tree and deleting its branch. Clean interactive quits drop the pane to an idle shell inside the worktree and leave reclamation to `rimz gc`. Signal exits start the helper with null stdio in its own process group, so cleanup can finish after the closing pane disappears. The helper, decision table, and `rimz gc` sweep are [worktree.md → Cleanup](./worktrees.md#cleanup).

**Run-pane cleanup.** After a blocking `-p` run finishes, pane cleanup is best-effort: RimZ closes the recorded launch pane, falling back to finding the agent row by `(kind, agent_id)` in the snapshot. A background run (`-p --bg`) passes cleanup to the in-pane wrapper: unless `--keep` was set, the wrapper watches the run record, terminates the agent once the run is terminal, performs marked-worktree cleanup, and closes its own pane. An operator stop or Ctrl+C-canceled blocking caller uses the same terminal record and wakeup path, then closes the recorded pane if it lingers past a short grace — reclaiming a kept run's pane whether the ref is the run id or the agent name.
