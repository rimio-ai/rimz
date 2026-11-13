# Workflow CLI

`rimz workflow` starts, inspects, and steers durable workflow runs: a sealed graph of checks and steps under `~/.rimz/workflows/<name>/`, driven by the run record rather than by any agent's memory. The [workflows guide](../../guide/workflows.md) teaches the format and a full example; how the engine decides is [workflow internals](../../internals/harness/workflow.md).

```sh
rimz workflow schema                              # the spec's JSON Schema
rimz workflow validate                            # every machine workflow
rimz workflow validate ~/.rimz/workflows/babysit-pr --json
rimz workflow run babysit-pr                      # in the current checkout
rimz workflow run babysit-pr -w feat-x --prompt 'merge before Friday'
rimz workflow list                                # active and parked runs, every room
rimz workflow show 'babysit-pr#feat-x'
rimz workflow show 'babysit-pr#feat-x' --history     # detailed history, folded repeats
rimz workflow show babysit-pr                      # definition rail, then live runs
rimz workflow show babysit-pr pr                   # one definition node in full
rimz workflow resume 'babysit-pr#feat-x' --at pr --extend visits=+1
rimz workflow restart 'babysit-pr#feat-x' --continue
rimz workflow cancel 'babysit-pr#feat-x'
```

The [global flags](../cli.md#global-flags) apply. Every subcommand except `run` works from any directory: a run id resolves across every room on the machine.

`show`, `resume`, `restart`, and `cancel` accept a run address `<workflow>#<lane>`, a workflow name, or a full run id (`wf_` and 32 hexadecimal digits). The lane is the basename of the recorded scope path, even after that checkout is removed. Root-checkout and project-scoped runs use the project directory's name, and `main` or the project path also names them. A requested `feat/x` also matches `feat-x`. A target with `#` but an empty workflow or lane, or with a second `#` in its lane, is a usage error (exit 2). When a target begins with `-`, put options before `--` and the target after it.

An address selects its live run; only `show` falls back to the newest finished run when none is live, so `resume`, `restart`, and `cancel` reach a finished run only by its id. Two live runs sharing an address refuse and list each id and checkout with a command for it. A workflow name in `show` draws its definition, then lists that workflow's live runs, never one run's detail; a known workflow with none ends with `no active or parked runs of <name>`. For `resume`, `restart`, and `cancel`, a workflow name prefers the caller's checkout or its project's project-scoped run, then the only live run, otherwise lists commands to choose one. Matching runs in the caller's room take precedence; without a local match, several rooms refuse with `--root` commands. Each command a refusal lists names its run by address, or by id where another candidate in the same room shares that address, and carries `--root` when the run is outside the current directory's room. The global `--root` chooses the caller's room. Run ids remain unambiguous across rooms.

## Run a workflow

`rimz workflow run <NAME|DIR> [-w <WORKTREE>] [--prompt <TEXT>]`

| Argument or flag | Meaning |
| --- | --- |
| `<NAME\|DIR>` | A single path component is a name under `~/.rimz/workflows/`; anything else is a directory, which must exist inside `~/.rimz/workflows/` (otherwise `<NAME\|DIR>: directory must exist inside <home>`). `.` is the current directory. |
| `-w, --worktree <WORKTREE>` | Select an existing RimZ-owned worktree instead of the current checkout. It never creates one. A workflow with `scope = "project"` refuses it. |
| `--prompt <TEXT>` | Stored unchanged, appended to every agent step's prompt, and exposed as `prompt` in `context.json`. The value may begin with `-`; for a value spelled like a flag (`--help`), write `--prompt=<TEXT>`. |

Before creating a run, the command validates the definition and refuses on any error, printing each as `<path>: <message>`. Warnings alone do not refuse; each prints to stderr as `warning: <path>: <reason>` before starting. It needs an existing room (`rimz start` creates one). With `scope = "worktree"`, nodes run in the selected checkout; with `scope = "project"`, they run at the project root, even when invoked from an owned worktree. One active or parked run holds each `(workflow, scope path)` pair, and a run a loop task started also holds `(task, scope path)`; a second start refuses and names the existing run. The CLI validates before it asks for that slot, so a broken definition still refuses on validation first. On success it prints the run id, then the run's work directory (`RIMZ_WORKFLOW_DIR`), then `→ rimz workflow show <run>`. The first two lines keep their scripting format. The first advance starts in the background. The work directory includes `notes/`, a link to the run's kept notes folder, and `signs/` for step guidance. Every evaluation and attempt takes a new `seq`, a retry included, so name a notes write after the item it records to make a repeat overwrite it.

Before it writes anything, `run` opens the home-wide start lock, `~/.rimz/loops/throttle.lock`, for write without taking it, since the background engine opens the same file on its first launch; `resume` and `restart`, including `rimz agents restart` on a workflow agent, do the same. When the open fails, the command exits 1 with `nothing was started` (`resumed`, `restarted`) and the cause, and leaves no run, record, or journal change behind. A read-only RimZ home, for example inside a provider's command sandbox, adds `this command cannot write the RimZ home` and the [sandbox remedy](./wait.md#arm-a-wait), worded for `rimz workflow`. The check covers RimZ's own files only: a check script that writes elsewhere, such as a tool's cache, can still fail inside a sandbox.

`-w` on a project-scoped workflow refuses before anything is created, with `<workflow> has scope = project, so it runs at <project root>; start it without -w` (exit 1). A loop task's fire into a worktree is not a `-w` start and keeps running at the project root.

### Failure handling with on-break

`[workflow] on-break` fills failure routes not explicitly set by a node. Use `on-break = "@failed"` (or `"@done"`) to end silently, or `on-break = { agent = "debugger" }` to open a staying agent with RimZ's debug trace. `debugger` is an example profile name, not a built-in profile. Add `prompt-file = "prompts/break.md"` inside that table to use your own prompt, confined to the definition directory; `agent` accepts the same layout grammar as an agent step.

The filled edges are check `timeout` and `invalid` (including judges and sleeps), agent-step `invalid`, message-step `undelivered`, step `cap`, and the workflow `deadline`. A step's timeout still follows `then`, or the step's `on.failed` when set. Explicit routes win. A judge retries a failed evaluation once, then follows `on.invalid`. A judge whose resolved account may not spend (the fleet's or the account's daily cap is spent, or the account is parked) never spawns, and the run ends `failed`; a loop task records `budget skipped`. Agent launch failures, including a failed continued restart, count as attempts and follow step `on.invalid`; a pre-launch provider limit still follows `then`, or the step's `on.failed` when set. A fresh launch whose takeover could not stop the checkout's occupants is first retried once: the step waits as [`retry`](#the-waiting-line) with no attempt or visit counted, the run's next advance launches again, and only a second failure counts. Each new entry into the step gets its retry back. A command takes no `on.invalid`, and a sleep's `on` table takes only `on.invalid`; any other key refuses.

An agent handler appears as the reserved step `@break`, with `lifecycle = "stay"` and `then = "@failed"`. Its input carries the original failure object plus `edge` (`timeout`, `invalid`, `cap`, or `deadline`) and `from` (the source node). The default prompt includes the run address and definition, failure, visits, a bounded excerpt of recent source history and the latest check answer or error, counts, a journal tail, and paths to the full journal and `context.json`. Judge evidence uses `.check.json`; tests and scripts use `.check.log`; conditions and sleeps have no output file. It tells the agent to fix and resume, cancel, or wait; urgent questions go through its own native UI. RimZ never stops this agent: resume and cancel detach it, leaving its pane open. This run excludes its detached debuggers from checkout occupancy and takeover eviction. Closing the pane ends the run as failed. Deadlines do not interrupt `@break`, and its own launch failure ends as failed.

`rimz workflow resume <run>` at `@break` re-enters `input.from`; `--at <node>` chooses another node, but cannot name `@break`. Resuming a cap break resets the source step's visit count, including with `--at`. Resume refuses while a live owner is settling an ending. Other active runs still accept only `--extend`. An unset `on.asked` keeps the agent's native question open. An unfilled failure route has a runtime floor of `@failed`, including a deadline first granted with `resume --extend deadline=…` when the definition had neither a deadline nor on-break. Filled edges have JSON `source = "on-break"`; explicit and default sources keep their meaning.

A definition cannot route to `@user`, even on an otherwise dead route. `validate`, `run`, and loop fires refuse it at its dotted path: route to a node, `@done`, or `@failed`, or set `[workflow] on-break`. `@break` is reserved for the generated handler, both as a node name and as an authored target.

Each fireable failure edge needs an explicit route or on-break: check `on.timeout` and `on.invalid`, agent-step `on.invalid`, message-step `on.undelivered`, step `on.cap`, and `workflow.on.deadline`. The refusal names that path and both fixes. An absent check timeout, visit cap, or workflow deadline makes that bound unfireable, so it needs no route. An on-break string other than `@done` or `@failed` refuses, and so does an agent table with a key other than `agent` and `prompt-file`.

### Steps and checks

The step leader's Workflow launch note describes its listed checks in order. Scripts and shells show their command and optional summary, explain their execution/path rules, and say the agent may run them itself. A judge shows only its summary, never its prompt filename; a sealed judge without one gets a fixed agent-judge description.

`[workflow] start` must name a check or a step, not a special target such as `@done` or `@user`. A step at the start runs through the same admission as any later visit. Its first `context.json` has `input`, `key`, and `prev_attempt` set to null, with `visit` and `visits` both 1.

Workflow agents, judges, tests, scripts, and commands receive `RIMZ_WORKFLOW_RUN` (run id), `RIMZ_WORKFLOW_DIR` (work directory), and `RIMZ_WORKFLOW_DEFINITION` (the absolute folder of the run's sealed definition, `definition/` in its run home). Every workflow agent gets an Environment `- workflow:` line even with `env_reminder` off. Non-workflow agent launches unset all three variables. If the agent launch cannot read the run record, it warns and omits the definition variable and line.

Every work node requires `then`: for an agent a check or a nonempty list of checks, for a command one check, for a message exactly one node, `@done`, or `@failed`. A `turn` or `stay` agent may instead set `then` to `@done` or `@failed` alone, ending the run when the attempt ends. A command requires `timeout`. On a `command`, `test`, or `script`, `run` is a literally passed argv, with slash-containing words confined to the definition directory, and `shell` runs through `sh -c` without path confinement; which one a node sets decides only how its command starts. Each kind takes only the fields its [table](#definition-fields) lists, so a command never waits for checkout occupancy or capacity. Their stdin is closed, both streams go to `work/<seq>.step.log`, and they inherit the workspace pin and workflow environment.

A message step's `message` is an agent address, `@<handle>` or `@<handle>#<channel>`; `@user`, `@me`, a bare name, and a pane id refuse at `message.<name>.message`. `prompt-file` is optional and confined. A message never reads occupancy, windows, capacity, or the start throttle. Each visit composes a header line naming the workflow, step, and run, followed by the prompt file, signs, and run prompt, as an agent step's prompt is composed; it resolves the address from the run's checkout among living agents and queues one automated `Type: WAIT` message from `@rimz` at gate `done`, sent at once to a resting agent. It records `ended` with the target's handle and follows `then`. No single match, or a prompt that cannot be composed, records `died` with the reason and follows `on.undelivered`, filled from `on-break` when unset; `@user` and `@break` refuse there. `session = "<agent session id>"` pins the step to one session: when the address resolves to an agent on another session (a handle since reused), the visit records `died` with `<address> is session <id>, not the pinned session <pin>` and follows `on.undelivered`. A lost host sends the same message id again on the next advance, and the queue keeps one copy and its first target. A cancel, deadline, or bound that ends the attempt before it settles retires the message if it is still queued; a resting target may already have received it.

An agent step requires `prompt-file`. Its `agent` accepts the fresh-launch layout grammar, including agent and command cells. `lifecycle` defaults to `until`: check `then` at each eligible leader turn end and leave the step when its route is neither this step, `@wait`, nor another listed check, closing the layout by default. Set `turn` for one turn, or `stay` to keep the attempt live until the leader's session exits. A staying step (`stay` or `until`, including an omitted lifecycle) refuses `on.asked`, and a `stay` step also `timeout`; set `lifecycle = "turn"` to use them. An `until` step's `timeout` bounds the whole session from its launch, less time spent on blocking asks; while the run is held at a check, that check's own timeout applies. The timeout ends the session as `timeout` and follows `then`, or `on.failed` when set. An `until` step cannot use a sleep as its `then` check; `turn` permits it. An `until` answer routed back to its step becomes the next prompt in that session, updates input/key and spends one visit; a cap follows `on.cap` instead, closing the agent by default. A held `@wait` spends no visit and rechecks on subscribed signals and the minute tick while the leader rests. A new turn abandons that wait and any check in flight. A staying step's `subscribe` takes `wake` entries, with the same defaults read for the checkout its leader launched in (its `worktree` when set), and also the team lifecycle signals (`team.idle`, `team.waiting`, `team.failed`, `team.ended`), which default to the team's cohort in that checkout, and `agent.*` selectors whose `match` names another agent by `handle` or `session`; the leader's own `agent.*` signals never reach it. It queues each matching signal for its leader as a `Type: SIGNAL` message from `@rimz` at the leader's next turn boundary, sent at once when it rests. The message opens with the signal's subject and details, then names the workflow, step and run. A table entry may add `prompt = "..."`, which follows after a blank line in each message that entry delivers. Each entry that matches sends its own message; entries with the same signal and `match` send one. A forward changes no run state, and a check subscribed to the same signal still wakes. Forwarding stops when the attempt ends, and a forward still queued then is canceled at delivery. A `turn` step refuses `subscribe`. A `stay` step may name a team: it launches the team's cohort the way a `stay` loop task does, its leader as the prompt leader. A team in a `turn` or `until` step refuses, naming `lifecycle = "stay"` as the fix. Command-only layouts and ambiguous prompt leaders refuse with a field-specific fix. Judge checks still take one profile. An agent step's `admission.window` gates every agent cell's provider.

An agent step's launch settings apply to each fresh launch as the matching `rimz agents` flags would: `account` (a login name, pinned on every cell; capacity admission counts the launch on that account and skips availability routing), `mode` (`auto`, `ask`, or `yolo`; `plan` refuses), `effort`, `fast` (true or false), `budget` (the `--budget` grammar, such as `"$5"` or `"$20/day"`), `system-prompt-file` (a file confined to the definition directory, refused at `agent.<step>.system-prompt-file` otherwise), and `worktree` (a worktree name the launch creates or reuses, empty for a generated name; the agent then starts there instead of the run's checkout). A restart that resumes a session keeps its account, system prompt, and checkout, and applies the rest. `on.failed` on an agent step takes an attempt that ended `died`, `timeout`, or `limited` (a launch the budget refused, or a run it stopped); a clean end, and every ending of a step without it, follows `then`.

`then = ["tests", "review"]` enters the first listed check. Every entry must be a check, and each except the last must have a direct route to the next. Empty lists refuse; until lists cannot contain sleeps. A route between listed checks on a held until session keeps it open, preserves input/key, resets check retry and starts the destination check's clock. The next eligible turn end starts again at the first check. A list of two or more checks on a `turn` or `stay` step warns that only an until step keeps its agent open across it. A listed check reached without a held session routes normally.

Failed or interrupted turns, live provider limits, and budget parks keep every workflow agent step open. A turn step finishes on a later clean end; an until check judges only a cleanly rested turn. Providers that report an interrupt as a clean end still let the check judge. A turn step's timeout pauses during blocking asks, limits, errors, and budget parks, counting overlaps once; silence and interrupts do not pause it. Deadlines never pause. Provider exit still follows `then` as `died` for a turn run or `ended` for a registered staying run. A budget refusal before launch still follows `then` as `limited`.

`keep = true` on a `turn` or `until` agent step leaves the leader and companions open, outside the workflow, on a natural attempt end or a closing check route. Default false retains cleanup. Cancel, restart, deadline, and a turn step's timeout still stop them; a push-back cap detaches them. A `turn` check returning to the step can leave the old agent beside the new one; occupancy still applies. A `stay` step refuses `keep`, including an explicit false value.

Agent steps default to notes on. Their launch brief names `<step>-notes.md` in the run's kept notes folder as working memory across visits, restarts, and subagents, including a record of changes outside the repository. `notes = false` omits that instruction; only an agent takes `notes`. New runs link `work/notes` to their dated folder under the room's `automation-notes/`; `gc.automation_notes_retention` defaults to keeping closed folders forever, independently of run-home cleanup.

A check's `on.<outcome>` is a target string: a node, `@done`, `@failed`, or `@wait`. A table or any other value refuses.

A sleep takes required `sleep` (nonzero `s`/`m`/`h`/`d` duration, at most `365d`) and `then` (node, `@done`, or `@failed`, never `@wait`), optional `when` (a nonempty ANDed loop-condition array) and `for` (a continuous hold shorter than `sleep`). It may take `provider` and `account` for window terms. Expiry follows `then` even during a hold; a true condition follows it early. Unread data, including under negation, never wakes it. Re-entry or a parked resume restarts its clock. A sleep forwards its incoming input and key except when routing a failure to `@break`, which receives the failure object, and needs no timeout. An until step's `then` cannot name a sleep.

### Limits and capacity

`[workflow.limit]` limits the agent steps that runs of that workflow name have going across rooms, and refuses with `set max-parallel or stagger` when it has neither field below. An agent's `[agent.<name>.admission]` takes the same two fields and limits that step's starts. An unknown key or an invalid value refuses:

| Field | Rule |
| --- | --- |
| `max-parallel` | Integer at least 1. Counts one per unfinished run whose record holds an agent step attempt, including an idle staying session, an `until` session held under its check (counted under its own step), and an attempt whose agent has not registered yet or has died, until an advance settles it. A live `@break` debugger holds its run's one slot like any staying step. Companions, RimZ-launched children and peers, headless judges, and a kept agent or `@break` debugger whose attempt has ended do not count. A step with companions counts as one, so a layout wider than the limit still starts. |
| `stagger` | Positive duration in `s`, `m`, `h`, or `d`, at most `365d`. Minimum gap between launches at this scope, stamped at launch and shared across rooms and restarts. |

Limits use each run's sealed definition and count runs of that workflow name, so a finished run frees its slot even when it kept its agents open. Waiting, paused, hibernated, and compacting sessions count while their run holds the attempt, and so does a parked run's. The incoming start counts as one, and a same-workflow start admitted but not yet launched holds the next one until its launch is reported. Limits apply after window and occupancy, before the two machine caps and other machine checks, even with no `[throttle]` table. No workflow or step limit bypasses a machine cap. Earlier Active capacity waiters held by a workflow limit go first across all steps; a step-limit waiter holds only its step. Ties use run id. A freed slot waits for the earlier run's next retry. Limits never stop an existing turn or pane.

Agent steps of automatically loop-started runs also ask the [start throttle](./loop.md#the-start-throttle) before taking over a checkout. `admission.throttle = "off"` skips the machine turn, stagger, resource limits, and capacity check, not the machine caps or applicable workflow and step limits. `admission.give-up` accepts a positive `s`, `m`, `h`, or `d` duration, or `forever` (the default). A held step spends no visit and retries about once a minute. Giving up follows `on.capacity`, default `@failed`, for machine and workflow limits alike; it may instead name `@done` or a node, but not `@user` or `@wait`, since capacity give-up never parks a run. A window or occupancy wait restarts the capacity clock. The run deadline still applies. Runs created by `workflow run` or `loop fire`, and runs detached by `loop remove`, skip waits (a limited step whose start queue stays busy for 30 seconds is held until a later tick) but claim any applicable workflow limit, count as automated afterwards, and restart workflow and step stagger, never machine stagger. Without limits these starts take no ticket. Checks, command steps, and already-open staying sessions never ask for admission.

Limit holds are self-clearing (`workflow_limit` or `step_limit`), shown last in their workflow group. Reasons start with `workflow <name>: ` or `step <name>: `, followed by `N of M agents open (@a, @b, +K)`, `next start in <duration>`, `N earlier starts waiting` (singular `1 earlier start waiting`), or `N starts launching` (singular `1 start launching`) while a launch of the same workflow is not yet counted. Names list at most two handles, then `+K` for the rest; the suffix is omitted when empty.

## Inspect definitions

`rimz workflow show <WORKFLOW>` draws the current definition in walk order, with returning steps hung beneath their earlier sending check. Checks draw `◇`, steps `○`. `→` points to a later node and `↑` to an earlier node or itself. Unreached nodes follow under `unreachable from <start>`. The header names the summary, scope, loop tasks that start this workflow in the caller's project (machine tasks only outside a room), any workflow `limit`, and any configured deadline route. Without a loop task it gives a `rimz loop add` command and the by-hand `rimz workflow run` command.

Each node shows its runner, summary when set, routes, and routing limits. Condition cases retain their order and hold durations, followed by `none true`; the note names where an unread term goes. A message step's runner is its target address. Timeouts, visit caps, windows, `takes over` or `shares the checkout`, bounded `give-up` waits, and staying lifecycles stay beside their nodes. Edges that cannot fire are omitted; uniform turn-end, invalid-check and unset-deadline defaults are stated once in the footer, only when used. `validate` shows the omitted edges with their marks. The footer names the source file and a command for one node in full. The live-runs table follows the definition.

A missing or unloadable definition prints `<workflow>: definition unavailable: <error>; check with rimz workflow validate <workflow>`, then still lists its runs and exits 0. An unknown workflow with neither a definition directory nor runs refuses. `show <WORKFLOW> --json` keeps its live-runs array and does not include the rail.

`<WORKFLOW>` also names a loop task in the caller's project that compiles to an inline graph (every task except an unguarded `--wait` and a `workflow =` task). `show` draws the compiled graph with the task's prompt and judge files materialized as assets, the footer reads `compiled from loop task <task>`, and the live-runs table lists that task's runs. A name that is both a deployed workflow and such a task refuses (exit 2) and names both selectors: `workflow:<name>` picks the deployed workflow and `task:<name>` the task, in `show`, `show <WORKFLOW> <NODE>`, and `validate`. `task:<name>` for a task that compiles to no inline graph refuses with `no loop task <name> compiles to a workflow` (exit 1).

`rimz workflow show <WORKFLOW> <NODE>` prints one definition node in full: runner kind and summary, predecessors with the outcomes that reach it, check subscriptions (derived for conditions, explicit otherwise) and the minute tick, a step's `receives` subscriptions with their resolved scope (`for this checkout`, `for this checkout's team board`, `where k=v`, or `anywhere in the room`), ordered condition cases with holds, every resolved edge with its source and fireability mark, then the configured command text, message target, agent layout, prompt path, lifecycle, occupancy, keep, window, throttle, limit, give-up, visit bounds, timeout, provider and account. A limit reads `max-parallel=N · stagger=<duration>`, omitting unset fields; the definition header prints its workflow limit in the same form. Commands keep their full text, including multiple lines. Never-firing edges appear here even though the rail omits them. Predecessors omit never-firing routes; conditional ones carry the shared mark.

The two-positional form takes a workflow name only, not an address or run id (usage error, exit 2), and conflicts with `--json` and `--history` in clap. An unknown node refuses with the definition's node names (exit 1). A missing or unloadable definition refuses with its validation command (exit 1); there is no runs-table fallback for a node view.

## Inspect runs

`rimz workflow show [RUN] [--history | --json]` defaults to the calling agent's workflow run, including from its subagents. Outside a workflow, an omitted RUN refuses with exit 1 and points at `rimz workflow show <run>` and `rimz workflow list`. The node view still requires an explicit workflow name. When a run's sealed definition cannot load, the human view and `--history` refuse with the load error (exit 1), and `--json` still prints the record and journal.

The compact view names the absolute kept notes folder after the work directory when the run has one. On Done, Failed, or Canceled, RimZ atomically writes the colorless `--history` view to that folder's `history.txt`. Terminal history uses the recorded end in the headline and date-qualified local clocks (`YYYY-MM-DD HH:MM`) on every row, so a later view or GC backfill reproduces the saved text; compact views continue to age. A failed copy warns without failing the committed transition. GC backfills a missing copy before removing the terminal run home and keeps the home if copying fails. Runs without notes write no copy.

History prints prompt and script digests as their first 12 hex characters and omits empty stopped/live child groups. When unread fields share one cause, their reason lists the fields and names the cause once. JSON retains full digests and recorded reasons.

When a manual step start skips a workflow or step limit hold, `show` prints `<hold>; started anyway: manual starts do not wait` for its current session, including while held at an until check. `--history` prints it inside that attempt's block after settlement. The first skipped hold is sampled at the admission claim, including for restarts and later steps, not at `workflow run` or `loop fire`; those commands print no such line. It may name an in-flight launch, earlier waiter, full cap, or stagger gap. A clear limit or failed limit read adds no note. JSON carries the text in optional `started_past_limit` on the live attempt and the journal attempt; absent values are omitted. Scheduled attempts never carry it.


`rimz workflow list [--json]` prints every active and parked run across rooms, without column headers. NEEDS YOU holds parked runs, active runs with a blocking ask, and capacity holds needing a fix (`check_failed`, `disk`, `store`), longest waiting first, with full addresses and a `workflow show` hint for its first run. The rest group by workflow name, each heading reading `<workflow> · N active[ · M held]`. Self-clearing capacity holds sort last in their group; each part sorts by address. NEEDS YOU rows are `<glyph> <address>  <detail>  <age>`: `?` for an agent's blocking ask, `⊘` for a capacity hold needing a fix. The latter uses the `workflow.held` glyph in both sets, also in the `show` headline and current rail node; it is not an agent question or a paused agent. Grouped rows are `<glyph> #<lane>  <detail>  <age>`: `◉` for self-clearing capacity holds, `⢿` for with-you sessions and active work, `☾` for other active waits, from the configured glyph set. A group names `started by <origin>` when all its runs share the loop task and manual-fire flag; otherwise each row adds ` · by <origin>`. An origin is `hand` or `loop task <task>[, fired by hand]`. Age starts at the park, at the opening of an ask, at the first capacity hold, or at node entry otherwise. Detail truncates before age at narrow terminal widths. Each row whose sealed definition loads is followed by the horizontal rail described below. Several rooms get headings (the home-relative project root), the caller's room first, and hints for other rooms carry `--root`. Finished runs are omitted. With no active or parked runs, it prints `no active or parked runs; start one with rimz workflow run <name>`, and `--json` prints `[]`.


Detail is `asking you: <handle> (<kind>): <one-line text>` for a blocking ask; otherwise a live staying session reads `with you in @<handle>`, with ` · checking <check>` while held and `: <wait phrase>` at that check, or ` · <check>: <wait phrase>` after it when waiting. The handle matches its agent card, without the channel already named by the row; if unavailable, it falls back to the recorded pet name. A capacity hold reads `held · <reason>`; other runs print the [wait phrase](#the-waiting-line), `running` (with ` · visit <n> of <max>` when capped), or `checking`. A cached PR number adds ` · PR #N` once per row; no cached number means no badge. Each room supplies its own agent names and PR cache. JSON reads neither that enrichment nor the journal: its keys and shape stay unchanged, and `detail` retains `with you` without an agent or PR and `held <age>[, gives up <clock>] · <reason>` for capacity waits, without node or the separate age column. A single-run `show` says `<step> is asking you` while asking, otherwise `<step> is with you` for a live staying attempt. Its session attempt is `held.attempt` while checking, otherwise `attempt`; its `lifecycle` is `stay` or `until`, omitted for turn attempts. An open blocking ask is stored in `asking`; `asked_ms` is the total closed pause time in milliseconds. Both are omitted when empty or zero. While held, the rail marks both the session step and its check current.

`rimz workflow show <RUN> [--history | --json]` opens with `<address>[  PR #N]`, then the activity glyph and current state on a second line. Only the state phrase takes the state tone; agent context uses body tone and the bound forecast is muted. A live agent uses its full card handle, including the channel, with the recorded pet name as fallback; `--history` keeps pet names. A with-you headline adds the same check/wait suffix as the list, without repeating it below. A wait names its node, elapsed time, recheck and the next bound's time and target; a hold names its outcome, duration and the next bound's time and target. The forecast picks the earliest unfired deadline, timeout or hold completion, in that order on ties. A running step shows its visit limit and agent handle when available. Asking reads `<step> is asking you · <handle> (<kind>)` and says to answer in the pane or with `rimz answer`; a turn step's timeout pauses while asking. A parked headline reads `parked at <node> <age> · <reason>`, leaving its explanation to the block below. Finished runs say `done`, `failed`, or `canceled` and show no PR badge. Their compact headline ages from termination; terminal `--history` uses the ending instant as its clock so its saved copy stays reproducible. The origin line names when and how the run started (by hand or by loop task), the number of completed attempts, and their known total cost, including judge evaluations. It omits cost when none is recorded, and never estimates a live attempt's spend.

On an active run, the current check's latest evaluation since entry supplies one saved readings line below the headline: `key=value` pairs separated by two spaces, null as `key=unread`, then ` · read <clock>`. Script and condition readings use the same line, with no per-case value annotations.

Alongside the saved readings, a human live or parked run shows one `now <check> · <outcome>` line for its condition check or sleep with `when`. On an agent step it reads the first listed check; on a parked check it reads that check. The line uses current `key=value` readings, null as `key=unread`, and the evaluator's unread reasons. A winning case with `for` shows the remaining hold from its open wait for that case, or the full duration without one. Forge keys add ` · read <clock>` for the cached probe; board-only checks omit it. The freshness floor is the step or parked node entry, or the held session entry while held. It launches no check, writes nothing and refreshes no probes; the CI fence may make read-only Git calls. Script, shell, judge and plain-sleep checks have no now line. JSON and history are unchanged.

The block below the origin shows the full ask text when asking. A takeover wait adds `takeover:` lines naming the agents that hold the scope path (`blockers unavailable` when the agent roster or the project's worktrees cannot be read). The `you can` commands a parked run prints, history footers, receipt hints and NEEDS YOU hints use the address when it selects this run, otherwise the run id, and add `--root` for another room. A visit cap lists the capped step's session ends, push-backs and the refused visit. A timeout shows the flushed wait as `<node> said <outcome>` with a folded count and span when present, and the last shell-check log; `invalid` shows the recorded stderr tail.

Active runs paint the definition rail after the block below the origin and before `visits`, using the run's sealed definition, never the edited source. Reached steps draw `●`, reached checks `◆`, with green glyphs and body-tone names. Unreached steps draw `○`, checks `◇`, in muted tone. Current nodes use the headline's glyph, bold in the state tone, including both the session step and check while held. Reached nodes add their last check outcome or completed attempt (outcome, duration, known cost, recorded HEAD before and after), its local clock, and `×N` for repeat entries. A capped step adds `visit <used> of <max>`, including the run's grant. Equal recorded commits read `HEAD <a>, no new commits`; different commits keep `HEAD <a> → <b>`. The current check names its outcome, `since <clock>`, `last checked <clock>` from its latest evaluation, wake selectors and the minute tick, with no advance count. A current staying step adds `receives` and its subscribed selectors. False cases are faint; the winning or held case reads `│ ▸ N`, bold blue except green for `@done` and alarm for `@failed`. The first unread case keeps `?` and `<key> unread: <reason>`. Later cases are unmarked and muted, except that `@done` and `@failed` cases keep their definition tone. A held case shows its elapsed and required duration and target, with a completion clock only when that completion is the next bound. Marks remain visible with `NO_COLOR=1`.

Parked runs put a horizontal rail after `you can`; finished runs also use it, with no current node. Steps draw `●` when reached and `○` otherwise; checks draw `◆` and `◇`. The current node uses the headline's activity glyph. Reached glyphs are green with body-tone names, current nodes are bold in the state tone, and unreached nodes are muted. A finished run's ending node is where it stopped: alarm-colored for failed runs, green for done or canceled runs. With colour off, the ending node has no mark of its own and draws as any other reached node, `●` or `◆`. Nodes join with ` ─ ` in walk order. A visited, current, or terminal-ending detour hangs below its sending check as `╰─ <glyph> <step>[ ×N] ↑`, with the visit total above one. Unreachable nodes appear after the main rail only when current or reached, including after `resume --at`. To fit a terminal, the rail drops nodes from the left as `… `, then from the right, regardless of whether they were reached, always keeping the current or ending node. Next, nodes between a held run's two current nodes drop as ` ─ … ─ `, and then the predecessor, which stays only while it fits. If that still does not fit, visited detours under the kept nodes drop, except those that are current or ending, and dropped nodes return wherever they fit. A detour drops with its parent unless it is current or the ending, which keeps its parent visible. Names are never shortened, so a current or ending node or detour wider than the terminal is drawn whole. Piped rails are not shortened. Capped visit totals remain in the `visits` rows below.

`visits  <step> <used> of <max>` lists each visited capped step, with the run's grant included in the limit, counted since its last manual restart or cap resume. A cap park lists the step's session ends and push-backs since that reset. Uncapped steps print no visit rows. The `trail` projects routing decisions from the sealed specification and journal, folding consecutive matching check answers as `×N`. It includes resumes, restarts, parks and termination, keeps at most the newest eight hops with `+N earlier` for the rest, and ends in `now` while live. The footer names the home-relative work directory and the command for full history.

`--history` replaces the compact view with the address, state headline, and detailed journal history in recorded order, using local clocks and readable facts rather than raw JSON. Live runs keep the compact view's headline and clocks; terminal runs use the absolute end and dated clocks described above. Check answers read `<node> said <outcome>` and, only when they routed, `→ <target>`, along with readings, reasons, shell logs, judge cost, and invalid stderr tails. An abandoned check reads `<node> said nothing · <outcome>` because it returned no answer. A route that repeats the preceding check answer takes no row. Attempts show visits, handles or command exits, duration, cost, short HEAD before and after, logs, reasons, stopped and live children, provider reset, questions, and digests. Other routes show targets and input fields; waits show outcome, count and span; turn ends show action, check, outcome and target. A park keeps its `park <reason>` marker but omits its text when the newest printed said or route row at the same node already printed that text as its object's `reason`; other parks keep their text. Resumes, cancellation, terminal states and wake signals retain their recorded facts. A forwarded signal reads `signal <name> → <handle>`, naming the leader it went to. With an explicit target, `--history` requires an address or id, not a workflow name; it conflicts with `--json` (usage error, exit 2).

Adjacent check answers with the same node, origin, outcome, and condition case index fold into one row, regardless of changes to their reason or other answer fields. Adjacent wakes fold when their node and signal agree; a wake between answers keeps its own row and breaks their run. A folded row uses the newest time, `#first–last` (or `#—` without sequences), `×N over <span>`, the newest answer, and the judge cost its members recorded, summed. The span runs from the oldest to the newest member; `over <span>` is omitted under one second. A closing check-wait summary takes no row when any earlier answer row at its node with its outcome overlaps the wait's inclusive span. Other waits, including step waits and waits whose answers are missing, print `×N over <span>` when N > 1, using the wait's first and last times and omitting the span under one second, followed by ` · <detail>` when present. Omitting a summary does not break an answer run. `--json` keeps every journal entry, including closing waits and duplicate routes, without folding.

### The waiting line

Status phrases print neither `said` nor an advance count, since an advance can find the run waiting without evaluating its check. The same phrase is the `detail` string in `workflow list --json` and `workflow_runs[].detail` in `loop list --json` and `loop show --json`. History and park answer rows keep `<node> said <outcome>` and their journal-fold counts.

A run at a wait carries a shared phrase without the node; capacity waits carry a held age. A single-run `show` puts the phrase in its state headline, after the check a `with you` headline names. Every instant, including capacity bounds, uses local `HH:MM` today, `Mon HH:MM` within six days in either direction, and `YYYY-MM-DD HH:MM` beyond that.

```console
$ rimz workflow list
review · 1 active · started by hand
  ☾ #project  pending, recheck 04:37  0s
      ☾ pr
```

| Part | Meaning |
| --- | --- |
| `<outcome>` | A check answered an outcome routed to `@wait`. |
| `holding <outcome> <elapsed>/<for>` | A condition case keeps winning while its hold duration runs. |
| `sleeping <elapsed>/<sleep>[ when <terms>] → <then>` | A sleep's entry-based clock, replacing every sleep wait phrase, including unread and held conditions, without a recheck clock. A plain sleep waits until its stored end; a conditional sleep re-samples about once a minute and on matching signals. |
| `unknown: <keys and reasons>` | A condition cannot read enough data to choose its first matching case. Bare `ci` can name a local HEAD not on the PR or CI read at a different PR head; see [JSON shapes](#json-shapes) for the reason forms. |
| `window <term>` | An agent step or judge whose provider `window` is closed, with the failing term and its reading. |
| `takeover` | The checkout holds an agent mid-turn, so the step cannot evict it yet. A step with `occupancy = "share"` never waits here. |
| `occupied` | The checkout holds an idle agent and the step's `occupancy` is `wait`, the default. |
| `retry: <reason>` | The step's launch failed stopping the checkout's occupants, before it started anything, and the next advance (the scheduler's minute tick or a hook wake) launches it once more; the closed wait stays in the history. A second failure is a counted `died` attempt. |
| `held <age>[, gives up <clock>]` | An automatic loop-started agent step is held by machine capacity or a workflow/step limit, or a limited or restarted step of any run is held by a start queue still busy after 30 seconds. Its give-up time is absent for `forever` and for a run that skips capacity waits; the reason follows ` · `. |
| `recheck` | The check's `recheck_at`, before which nothing evaluates; a signal matching that check's subscription clears it. For a `for` hold it is when the hold completes, and subscribed signals and the minute tick re-sample the case before then. |

## Cancel and resume

`rimz workflow restart <RUN> [--continue]` replaces the current agent step, including a held `until` session or a run parked at an agent step. It stops the owned layout, keeps input and key, resets that step's visit count, and starts at visit 1. It skips the step's usage window and capacity waits as a manual start (a busy start queue still holds it); a layout wider than a machine cap still refuses, and the step's checkout occupancy still applies. Other steps' counts and run-wide grants are unchanged.

Restart cancels any unconsumed push-back from the abandoned attempt, including one already sent to the pane, so it cannot delay judgment of the replacement's next clean turn. Other messages are unchanged.

Without `--continue`, the replacement starts fresh. With it, the leader resumes the current attempt's recorded conversation under the room's current account; from a parked agent step, it uses the previous attempt only when that attempt belongs to the same step. A still-live previous card is stopped before relaunch, and companions start fresh. A missing conversation, unsupported provider resume, or a different account history pool refuses before anything stops, naming a fresh `workflow restart` or `rimz accounts use` fix. If resume preparation fails after the stop, the run follows the step's `on.invalid`. These refuse without stopping anything: a finished run (`workflow run <id> is <state>; cannot restart`), a run being canceled, a check without a held session, a command or message step (`node <node> is <kind>[; @user: <park> | ; waiting on <outcome>]; restart requires an agent step`), and a restart already pending (`this step is already restarting`).

The command waits up to 30 seconds for the replacement to launch, park, or end the run, then prints one receipt line and a `workflow show` hint. The mode is `fresh` or `resumed (<session>)`.

| Outcome | Receipt |
| --- | --- |
| Launched | `restarted <address> step <step> as <handle> · <mode> · visits restarted at 1` |
| Parked | `restart <address> step <step> · <mode> · no live replacement: <park>` |
| Run ended | `restart <address> step <step> · <mode> · run is <state>; no live replacement` |
| Still pending (an occupancy wait, a stop in progress, or 30 seconds passed) | `restarting <address> step <step> · <mode> · awaiting launch; visits reset` |

`workflow show` follows a pending restart's progress. History records the canceled live attempt, a full visit reset, and `<step> restart · fresh|continued · visits restarted at 1`; stopping a previous attempt again does not repeat its end or cost.

The journal records `{event: "restart", step, continued}` and `{event: "visits_reset", step, outcome: "restart" | "cap"}`. A cap explanation starts after the step's last reset.

`rimz workflow cancel <RUN>` ends a run as `canceled` and stops its live script, shell, judge, or step attempt, except that it detaches the `@break` agent. It prints `canceled <address>`. An already-finished run id prints `<address> is already <state>; unchanged` and still exits 0. Both receipts end with a `workflow show` hint.

A run whose record another RimZ version wrote is not read: a command that reaches it refuses with `` workflow run <run> at <path> has record version <N>, written by another RimZ version; this RimZ reads only version 6. Clear it with `rimz workflow cancel <run>`, then start the workflow again ``, and `workflow list` prints that line on stderr after `warning:` for each such run. `rimz workflow cancel <run-id>`, given the run's id, removes its home without reading it and prints `removed <run-id>: another RimZ version wrote its record, so it was cleared unread; any agent it left running is untouched`. Background readers treat such a run as absent: the loop scheduler, the start limits' count of open workflow agents and worktree cleanup skip it, and `rimz gc` removes its home once its record is more than seven days old, unless a newer RimZ wrote it.

A cancellation journal row may carry `reason`; `show --history` prints `cancel canceled · <reason>` when present, otherwise `cancel canceled`.

An unfinished run holds its scoped worktree against removal, even while parked with no agent. `rimz worktree remove NAME --force` removes the checkout and then cancels its unfinished runs with reason `worktree removed`. A failed cancellation leaves the removal standing, exits non-zero, and prints the run id's `rimz workflow cancel` command to finish it.

`rimz workflow resume <RUN> [--at <NODE>] [--extend <GRANT>]...` puts a parked run back to work, at its current node or at `--at <NODE>`, with a fresh timeout clock and a cleared check retry. `--extend` repeats:

| Grant | Effect |
| --- | --- |
| `visits=+N` | N is a positive integer. Adds N to every step's `max-visits` for this run. The grant survives resets. |
| `deadline=<WHEN>` | Replaces the run's deadline and arms it again. `WHEN` is an RFC 3339 instant or a local date or date-time; a bare date is local midnight. |

Any other grant spelling refuses with `invalid extension "<grant>"; use visits=+N or deadline=<when>`. An active run accepts `--extend` alone (`an active run accepts only --extend; park it before repositioning`), except at `@break`, where resume detaches the debugger and re-enters `input.from` or `--at <node>`. `--at` refuses an unknown node (`unknown sealed workflow node <node>`) and `@break`. A finished run refuses (`terminal workflow runs cannot resume`). The definition stays sealed: grants live on the run record.

The receipt says `resumed <address> at <node>`, then ` · <node> <used> of <max> visits` when that step is capped, `, every capped step +N` for an added visit grant, and ` · deadline <clock>` for an armed deadline. Extending an active run says `extended <address>` instead, with ` · every capped step +N visits` and the same deadline part. Both end with a `workflow show` hint. Resuming a cap park resets the capped step's visit count, even with `--at`. When the armed deadline has already passed, stderr adds `warning: deadline <clock> has already passed; <destination> — <resume command>`, where the destination reads `the run goes to <target> at once`, and the command is the matching `--extend deadline=<when>`. A fired deadline stays disarmed on plain resume; use `--extend deadline=<WHEN>` to arm it again.

```console
$ rimz workflow resume 'repair#feat-x' --extend 'visits=+1'
resumed repair#feat-x at work · work 2 of 3 visits, every capped step +1
→ rimz workflow show 'repair#feat-x'
```

## Definition fields

A definition is `workflow.toml`: one `[workflow]` table and any number of nodes, each a table named by its kind, `[<kind>.<name>]`. A name is unique across all eight kinds. A field marked required refuses when absent; a field a kind's table does not list refuses there ([validation](#validate-and-schema)). Every node also takes `summary`, one nonempty line.

| `[workflow]` | Meaning |
| --- | --- |
| `scope` | Required. `worktree` or `project`. |
| `start` | Required. The first node. |
| `deadline` | A date, local date and time, or RFC 3339 instant that bounds the run. |
| `limit` | `max-parallel` and `stagger`, at least one ([limits](#limits-and-capacity)). |
| `on-break` | `@failed`, `@done`, or `{ agent, prompt-file }` ([on-break](#failure-handling-with-on-break)). |
| `on.deadline` | Where the run goes when its deadline passes. |

| `[agent.<name>]` | Meaning |
| --- | --- |
| `agent` | Required. A kind, profile, or layout; a team only with `lifecycle = "stay"`. |
| `prompt-file` | Required. A file confined to the definition directory. |
| `then` | Required. A check or a nonempty list of checks; on `turn` or `stay`, `@done` or `@failed` alone. |
| `lifecycle` | `turn`, `stay`, or `until` (the default). |
| `timeout` | A turn's bound, or an `until` session's from its launch; a `stay` step refuses it. |
| `max-visits` | At least 1. |
| `keep` | `turn` and `until` only. |
| `notes` | `false` omits the notes instruction. |
| `subscribe` | Signals forwarded to a staying leader; a `turn` step refuses it. |
| `on.asked`, `on.cap`, `on.capacity`, `on.invalid`, `on.failed` | Targets for a question (`turn` only), a spent visit cap, a capacity give-up, a launch failure, and a `died`, `timeout`, or `limited` ending. |
| `admission` | `window`, `occupancy` (`wait`, the default, `takeover`, or `share`), `max-parallel`, `stagger`, `throttle` (`off`), `give-up` (a duration or `forever`, the default). |
| `account`, `mode`, `effort`, `fast`, `budget`, `system-prompt-file`, `worktree` | Launch settings, as the matching `rimz agents` flags. |

| `[command.<name>]` | Meaning |
| --- | --- |
| `run` or `shell` | Required, exactly one. |
| `timeout` | Required. |
| `then` | Required. One check. |
| `max-visits`, `on.cap` | The visit bound and where a spent bound goes. |

| `[message.<name>]` | Meaning |
| --- | --- |
| `message` | Required. An agent address, `@<handle>` or `@<handle>#<channel>`. |
| `then` | Required. One node, `@done`, or `@failed`. |
| `prompt-file` | A file confined to the definition directory. |
| `session` | The agent session the address must resolve to. |
| `max-visits`, `on.cap`, `on.undelivered` | The visit bound, and where a spent bound and an undelivered message go. |

| `[test.<name>]` | Meaning |
| --- | --- |
| `run` or `shell` | Required, exactly one. Exit 0 answers `pass`, anything else `fail`. |
| `on.pass`, `on.fail` | Required. |
| `on.timeout`, `on.invalid` | Failure routes. |
| `timeout` | Required when a route waits. |
| `wake` | Signals that re-run the test. |

| `[script.<name>]` | Meaning |
| --- | --- |
| `run` or `shell` | Required, exactly one. Stdout is the JSON answer. |
| `on.<outcome>` | One target per outcome the script answers, plus `on.timeout` and `on.invalid`. |
| `timeout` | Required when a route waits. |
| `wake` | Signals that re-run the script. |

| `[judge.<name>]` | Meaning |
| --- | --- |
| `agent` | Required. One profile with a headless launch. |
| `prompt-file` | Required. A file confined to the definition directory. |
| `summary` | Required. |
| `on.<outcome>` | One target per outcome the judge answers, plus `on.timeout` and `on.invalid`. |
| `timeout` | Required when a route waits. |
| `wake` | Signals that re-run the judge. |
| `admission` | `window` only. |

| `[condition.<name>]` | Meaning |
| --- | --- |
| `cases` | Required, nonempty. Each case takes `when` and `outcome`, both required, and `for`. |
| `else` | Required. The outcome when no case is true. |
| `on.<outcome>` | Required for every case outcome and `else`. |
| `on.unknown` | Required. |
| `on.timeout`, `on.invalid` | Failure routes. |
| `timeout` | Required when a route waits. |
| `provider`, `account` | The window terms' agent kind and login. |

| `[sleep.<name>]` | Meaning |
| --- | --- |
| `sleep` | Required. A nonzero duration. |
| `then` | Required. A node, `@done`, or `@failed`. |
| `when`, `for` | An early end: an ANDed term array, and how long it must hold. |
| `provider`, `account` | The window terms' agent kind and login. |
| `on.invalid` | A failure route. |

## Validate and schema

`rimz workflow validate [DIR] [--json]` checks one directory, or every directory under `~/.rimz/workflows/` whose name does not start with `.` (a crashed [`loop eject`](./loop.md#eject-a-task) leaves its staging copy under `.eject/`). A single normal path component names a workflow under that home, as in `run`; any other path is checked where it is, inside that home or not, and its name must still be a valid workflow name. A name may instead select a loop task's compiled graph, as in `show`, with the same `workflow:`/`task:` selectors. Profiles and teams resolve from the machine's definitions either way. The table has `WORKFLOW`, `NODE`, `KIND`, `RUNNER` (the run command, shell text, step layout, judge profile, `message`, `condition`, or `sleep`), `BACKGROUND` (whether the step's leader reports background work), and `EDGES`. An edge prints `outcome → target (source)`, or `outcome → target (source, mark)` when conditional or unable to fire; `source` is `explicit`, `default`, or `on-break`. A step's `asked` edge appears only when `on.asked` is set, always as explicit; staying steps have none. Below the table, each error prints as `<path>: <message>`, then each warning as `warning: <path>: <reason>`, all on stdout. A definition that fails to parse reports one error, `<path>: <workflow>: <error>`. Any error exits 1 and adds `error: N workflow error(s)` on stderr; warnings alone exit 0. Condition and sleep stage names use the caller's effective teams plus `Done`; without a resolved workspace, a warning says stage names were not checked. Run admission always checks against the run's project.

Every table refuses a field it does not take, including a field another kind owns, at its dotted path with zero-based array indices, such as `agent.fix.takeover` or `condition.pr.cases.1.for`: `` agent.fix.takeover: <workflow>: agent.fix.takeover: unknown field `takeover`, expected one of `summary`, `agent`, … ``. A wrong-typed value refuses the same way, and so does a name two kinds share (`test.fix: <workflow>: test.fix: agent.fix has the same name; a node name is unique across every kind`). These are parse errors, so the definition reports the first one alone.

An invalid value in an optional field refuses at its path as a required field's does; nothing a definition writes is read as absent. That covers a blank or multiline `summary`, an unsupported `deadline`, a `timeout`, `stagger`, `for` or `sleep` that is not an `s`/`m`/`h`/`d` duration within `365d`, a zero `stagger`, `for` or `sleep`, an unsupported `window`, `max-visits` or `max-parallel` below 1, `give-up` that is zero, an invalid `wake` or `subscribe` entry (a bad selector or match, an agent lifecycle selector, a team selector other than exact `team.stage`, or a `prompt` on a check's `wake`), an unknown condition or sleep `provider`, a `provider` with no window term, an `account` without `provider`, an invalid agent `account`, `mode = "plan"`, a `budget` outside the `--budget` grammar, an invalid `worktree` name (empty is allowed), and a blank message `session`.

A check's route refuses when the check cannot answer it (`dead route: this check cannot answer that outcome`): a reserved outcome other than `timeout` and `invalid`, which a condition extends with `unknown`, or a condition outcome that no case or `else` declares. A route is a target string; a target that names no node, `on.timeout = "@wait"`, a work node's edge to `@wait`, `on.capacity` to `@user` or `@wait`, and `on.cap` targeting its own step also refuse. Cross-field rules read the definition as written: a check-only cycle needs `@wait`, a cycle through work nodes needs `max-visits` on one of them, and every failure route except the deadline's counts as an edge.

Rows follow the graph from `start`, with a returning step immediately below the earlier check that sends it. Nodes reachable only through `workflow.on.deadline` follow the start component; unreachable nodes come last in name order. Edges follow case order without repeats, then `else`, then other explicit outcomes in name order, then engine triggers: `unknown`, `timeout`, `invalid`, `deadline` for checks; `then`, `failed`, `undelivered`, `asked`, `cap`, `capacity`, `deadline` for steps. These row and edge orders also apply to JSON.

| Mark | Meaning |
| --- | --- |
| `never: no timeout` | The check has no timeout clock. |
| `never: no max-visits` | The step has neither visit bound. |
| `never: give-up is forever` | Capacity admission never gives up. |
| `only with a deadline` | No deadline is sealed; a run needs a deadline grant. |
| `only when a loop task starts the run` | Capacity can give up in an automatic loop-started run, not a by-hand run or `loop fire`. |

`--json` prints `{rows, warnings, errors}`, each error and warning carrying `path` and `message`. Each row has `runner = "run" | "shell" | "agent" | "message" | "condition" | "sleep"`, nullable `summary`, `run`, `shell`, `agent`, and `message` fields, `workflow`, `node`, `kind`, `do_background`, and resolved `edges`. Each edge keeps `outcome`, `target`, and `source`, and adds `fires`: `"yes"`, `"never"`, `"with-deadline"`, or `"in-loop-runs"`. Only `"never"` edges carry `because`: `"no-timeout"`, `"no-max-visits"`, or `"give-up-forever"`. A bounded capacity edge remains reachable in loop runs even with `admission.throttle = "off"`, because machine caps can hold it. Marks explain edges; they do not add validation warnings or relax admission rules.

A sleep's edges are explicit `then → <then>`, default `sleeping → @wait`, default `unknown → @wait` only with `when`, `invalid` filled by on-break, else `@failed`, and the header's `deadline`, in that order. There is no timeout edge. Definition rails and node details show `sleep`, `when`, `for`, and `then`, never the generated waiting edges or a generated case. A plain sleep journals `{outcome: "sleeping", recheck_at: "<entered_at + sleep>"}` at entry and `{outcome: "then", reason: "slept <sleep>"}` at expiry, inheriting the key into each object. Conditional sleeps use condition answers with `case: 0` for an early win; false is `sleeping`, unread is `unknown`, and an unfinished `for` rewrites a win to `hold`. These answers never replace the run's input or key. History hops show `sleeping → @wait` and `then → <then>`.

Each row's `kind` is the table word the node is written under (`agent`, `command`, `message`, `test`, `script`, `judge`, `condition`, or `sleep`), as is the `KIND` column. Command rows have `runner = "run" | "shell"`, the command in the corresponding field, `agent = null`, and `do_background = false`. Their edges are `then`, `cap`, and `deadline`, never `asked` or `capacity`; `BACKGROUND` prints `—`. Agent-step rows include a `capacity` edge to `@failed` by default, marked according to `admission.give-up`. Agent-only warnings do not apply to commands. Message-step rows have `runner = "message"`, the address in `message`, `agent = null`, and `do_background = false`; their edges are `then`, `undelivered`, `cap`, and `deadline`, and `BACKGROUND` prints `—`.

Turn and `until` steps warn when the leader adapter has no background reporting: a turn end may finish the attempt while provider background work remains. When auto-continue is off, agent steps warn at `agent.<step>` that a step paused on a provider limit waits for you, and judges warn at `judge.<check>` that a provider limit mid-attempt ends the attempt as `limited`. A Codex judge instead warns that a failed judge retries once, then follows `on.invalid`. The generated `@break` handler warns at `workflow.on-break`.

`rimz workflow schema` prints the JSON Schema of `workflow.toml`, derived from the loader's own tables, so an editor flags an unknown field (every table keeps `additionalProperties: false`), a value of the wrong type, and a field the loader always requires, such as an agent's `then`. Rules that depend on other fields or on the node's routes, such as `run` or `shell`, a condition's `timeout`, a judge's `summary`, and a test's `on.pass` and `on.fail`, are checked by `validate` alone. The `[workflow]` header and every node accept an optional `summary`: one nonempty line describing the graph or the node. A blank or multiline value refuses at its path. A judge requires one, refusing at `judge.<name>.summary` with `add summary = "..."`.


## Loop tasks

A [loop task](./loop.md) starts a workflow with `workflow = "<name>"` in place of `agent` or `wait`. `rimz loop add <task> --workflow <name>` writes it, after validating the definition as `workflow run` does; [its flag list](./loop.md#actions) names what it refuses. [`rimz loop eject <task>`](./loop.md#eject-a-task) writes one from a task's compiled graph and points the task at it. This task comes from `rimz loop add babysit-pr --workflow babysit-pr --each-worktree --when pr=open --for 3m`:

```toml
[tasks.babysit-pr]
workflow      = "babysit-pr"
each-worktree = true
when          = ["pr=open"]
for           = "3m"
```

| Field | Meaning on a workflow task |
| --- | --- |
| `workflow` | The definition name under `~/.rimz/workflows/`. Conflicts with `agent` and `wait`. |
| `prompt`, `prompt-file` | The run's prompt. |
| `each-worktree` | Expand each fire over RimZ-owned checkouts. With `when`, launch once per checkout. A worktree-scoped run without it scopes to the task's `dir`, else its root; a project-scoped condition launch covers all checkouts permanently, while other triggers skip only a live held run. |
| `when`, `for`, schedules, `root` | As on any task. |
| `budget` | Ignored on a hand-written row, and refused by `loop add --workflow`: each agent step sets its own `budget`. The turn, room, and account caps still park a step agent like any agent in the room. |
| `budget-per-day` | Gates each run the task starts: the day's reported run costs, plus the `budget` of every agent step in each live run of the task, plus the next run's own step budgets, must stay within the cap. A graph with no step budgets is admitted until the day's cost reaches it. Each run's cost reaches the task's history when the run ends. |
| `surplus`, `surplus-after` | Gate each run the task starts on the account of every agent step and judge in the graph. |

Loop-started runs do not print validation warnings. `loop show` joins each checkout to its recorded run and lists every run the task started, finished runs included; `loop logs` gives each launch block its run's state and `workflow show` command, and `loop fire` prints that command under its `launched <id>` receipt. `loop list` keeps only active and parked runs; its launched-worktree count includes finished entries.

`stay`, `takeover`, `subscribe`, `check`, `judge`, `throttle`, and `capacity-wait` are refused on a workflow task, each with the workflow field that owns the concept. A `when` task launches its workflow once per checkout: subsequent scheduled fires start nothing and record nothing while that run is live, and after it ends if it acted or was canceled; a run that failed or ended on its own without acting frees the checkout ([launch memory](./loop.md#launch-memory)). Project-scoped launches cover all of the task's checkouts, including later ones. `rimz loop fire <task>` bypasses this memory and replaces the entry only when a new run starts. `rimz workflow run` starts an independent run without changing the task's entry. Task replacement preserves the entries; rename moves them and removal clears them. Removal also detaches the task's active and parked workflow runs, which continue as independent runs.

An unrecorded checkout retries a sustained-true condition every five minutes. A fire that finds an active or parked run follows the task's [`--overlap`](./loop.md#overlapping-triggers) policy; the default records `overlapped` naming the run and adds no strike, checking before it reads the definition or prompt. If that run belongs to this task, the fire repairs its launch entry, so later scheduled fires are silent. A run started independently or by another task is not adopted, and the task retries until that run ends. Schedule, signal, and watch workflow tasks keep admitting a new run on each fire unless a live run holds the scope; finished runs do not block them unless the row sets `once-per-checkout = true`.

## Failure text

Script process failures say `<script> exited N` or `<script> killed by SIGTERM` (using the signal's name when known). `<script>` is the first `run` word containing `/`, or its first word when none does. Judge process failures say `judge exited N` or `judge killed by SIGKILL`; provider errors or invalid provider output take precedence over the exit status. Existing runs retain their recorded text.

Routes, step launches, parks, and ends append `workflow_*` assists ([stats](./stats.md#the-assist-timeline)). A merged wait appends one route assist when it closes, with its final `count`; an open wait has no route assist yet. Stats renders a closed wait as `workflow <workflow> <node> waited <outcome> ×<count>`.

## JSON shapes

`list --json` and `show <workflow> --json` print arrays of `{id, address, workflow, scope, state, node, since, detail}`, with optional `hold` for a known capacity cause. `scope` is the scope path: the checkout, or the project root for `scope = "project"`. `since` remains node entry, not the human row's state-specific age. `detail` is the shared phrase above, so its capacity bounds follow the same day-qualified clock as rechecks and passed deadlines. Read the node and its entry time from `node` and `since`, never from `detail`.

`show [RUN] --json` prints `{address, dir, record, journal}`: the short run address, work directory, run record, and history entries. Runs with kept notes add `notes`, the absolute folder path. When the sealed definition cannot load, it still prints the record and journal. An active attempt's optional `asking` is `{since, text, kind, handle}`, with `kind` one of `question`, `permission`, or `plan_approval`; `asked_ms` accumulates closed pauses. These fields pause a turn step's timeout, not the run deadline. Evaluation rows carry `origin = "check"` for a check's answer or `"engine"` for an engine-decided outcome. A forwarded signal journals `{event: "signal", node: <step>, signal: <name>, message: <message id>}`, with the subscribing step as its node even while an until session is held at its check.

Optional `attempt.held_since` opens the shared pause at the earliest ask, limit, error, or budget-park evidence, clamped to the attempt's start and the observation time. It remains open through overlaps; `asked_ms` credits the interval once all such evidence clears.

Only blocking native asks pause an attempt. Async questions and explicitly raised questions remain actionable in `rimz asks` but do not create `asking` or pause the timeout.

Optional `record.restart` is `{resume: "session-id" | null, settled?: N}` while a manual replacement is pending. Null means a fresh start. `settled` names a previous attempt being stopped again whose end and cost are already journaled. The field disappears when the launch is published or the run parks or terminates.

`record.visits.<step>` is the step's visit count, since its last manual restart or cap resume. Attempt journal rows and `record.prev_attempt` carry `routed_by` (the outcome that admitted the attempt) beside `key`, null when absent.

A capacity wait has `record.wait.outcome = "capacity"`, provider `kinds`, optional `hold` (the [typed cause](../../internals/harness/loops.md#the-start-throttle), absent in older records), and optional `gives_up_at` (absent for `forever`, and for a run that skips capacity waits), alongside the usual `node`, `detail`, `first`, `last`, and `count`. Closing the wait carries `hold` into its history entry. Giving up journals a `capacity` route with input `{outcome: "capacity", step, reason, kinds, waited_ms}` and any inherited `key`.

Runs use `record.version = 6`, and RimZ refuses a record of any other version ([`cancel`](#cancel-and-resume) removes such a run). While checking an open session, optional `held` is `{attempt, entered_at, verdict?}`: the session attempt, the step's entry time, and the saved check verdict. A step's `attempt.visit` records its visit count at admission, including that launch or push-back; the context, `live_attempt`, `prev_attempt`, and attempt journal row carry that number. An attempt without a visit stamp uses the current count. Optional `attempt.push = {message, queued}` persists the push-back message id before delivery; `queued` means the host has confirmed enqueue, including an id already in live or retained history. Optional `attempt.judged` is `{agent_id, started_at, ended_at}`, the consumed leader turn end. A verdict is `{node, entered_at, at, target, object, reason}`, where `at` is the instant its evaluation was journaled. A closing attempt carries the verdict in optional `attempt.ending.route` until the stop settles.

Each turn-end decision or held-wait answer writes a journal row with `event = "turn_end"`, the session's `seq` and step `node`, `action = "kept" | "pushed" | "closed" | "discarded" | "interrupted"`, `turn = {agent_id, started_at, ended_at}`, and `check`. A saved verdict supplies optional `outcome` and `target`; `reason` explains an interruption or a moved-turn discard. A `pushed` row also carries optional `visit` and `message` and counts as a visit in history, the trail and cap inspection. Text history prints the action and any outcome, and the target only for `kept`, `pushed` and `closed`, since a discarded or interrupted verdict routed nowhere.

`work/context.json` adds `live_attempt` while an `until` session is open with no staged ending: `{step, seq, visit, visits, key, started_at, handle, run_id, claim_path, head_start, turn_end}`. `turn_end` is the last consumed turn identity, or null. There is no outcome or end evidence in `live_attempt`; `prev_attempt` still describes the last ended attempt. During evaluation, the top-level context describes the check, without `visit` or `visits`; a push-back or discard rewrites it to the session's step view, a held `@wait` keeps the check view, and a close leaves it until the next publication.

Command-step attempts and `record.prev_attempt` carry nullable `code` (exit code, null after a signal or an unobserved exit) and `log` (the absolute `work/<seq>.step.log` path when the file exists). Agent attempts write both as null; records predating these fields still decode. Command steps have null `handle`, `run_id`, `children`, `limited`, `asked`, and `cost_usd`. A message-step attempt carries `handle` (the target, null when undelivered) and `message`, the id of the queued message, also on its journal `attempt` row; `code`, `log`, and `run_id` are null. Other attempts omit `message`. The hosting process samples `head_start` before the command and `head_end` after reaping it; `head_end` is null when another process settles it. `claim_path` still names `work/<seq>.claim.json`.

A command step records `ended` on exit 0, `died` on another exit, signal, spawn failure, or log-open failure, and `timeout` when its step timeout interrupts it; these follow `then`. A run deadline records `canceled` and follows `on.deadline`. Cancel records `canceled` and ends the run. A lost host records `canceled` with a reason and follows `then`, without rerunning the action. An interrupted command whose bound is no longer due follows `then` as `canceled`. A child that exited on its own keeps its exit outcome even if a bound fell due before settlement. The next check receives the original routed input; the command's evidence is in `prev_attempt`.

Text history keeps the attempt event line and adds `    exit <code|—> HEAD <start8|—> → <end8|—>` for a command step, followed by `    log: <path>` when present. An agent attempt names its handle instead of the exit. Any attempt with a reason adds `    reason: <text>`. A running command's headline has no agent handle.

A visit cap routes `on.cap` with `{outcome: "cap", step, bound: "max-visits", visits, max, routed_by: "name" | null}`, plus the inherited `key` when present. `routed_by` is the outcome that tried to enter the step. Resuming a cap park resets that step's count, while `visits=+N` still raises the limits run-wide. `visits` is the step's count; `max` is its limit plus the run's visit grant. The journal's route input carries the same object.

A test's answer is `{outcome, code, log}` plus an inherited `key` when present. Exit 0 is `pass`; another exit or signal is `fail`. `code` is an integer, or null after a signal. `log` is the absolute `work/<seq>.check.log` path, containing stdout and stderr together. Only `fail` has `reason` (`exited N` or `killed by signal`); output tails are not journaled. Both `on.pass` and `on.fail` are required. The only additional routes are `on.timeout` and `on.invalid`, filled by on-break or the `@failed` floor; command spawn or log-open errors are `invalid`. The check's optional timeout spans waits and evaluations, without inheriting the loop's default timeout. A timeout routes `timeout`; a run deadline follows `workflow.on.deadline`. Both stop the command's process group rather than answering `fail`. A `fail` routed to `@wait` shows `said fail` in the waiting line.

A script keeps stdout as its JSON answer and writes complete stderr, including an empty stream, to `work/<seq>.check.log` once it settles. Invalid answers still journal the stderr tail. A child check's until push-back names its answering check, then its reason, then `output: <path> (<size label>)`, or `no output` for an empty or missing file, before the closing instruction. Tests and scripts use `.check.log`; a judge uses its whole saved `.check.json` answer. No tail or file content is copied into the prompt. Condition push-backs keep their term explanations.

A condition's answer has `outcome`, `readings` (a map from term key to string or null), and the winning zero-based `case` (omitted for `else` and `unknown`). The inherited run key is included when present; cases do not set keys. `unknown` includes a `reason` naming each unread key. While a case's `for` hold runs, the outcome is `hold`; `record.wait` carries `case`, `detail` (the case's outcome), `first`, `last`, `count`, and `recheck_at`. Without a hold, `wait.case` is omitted. An `unknown` answer routed to `@wait` keeps a running hold, its `first` included, while the held case reads true or unknown, and sets `wait.unknown` to that answer's reason (empty when it gives none) until a true answer of the held case clears it; the hold closes when that case reads false, another case answers, or a route leaves the check. A condition routes `unknown` only through its required `on.unknown`, and a conditional sleep's `unknown` waits; `timeout` and `invalid` use on-break or the `@failed` floor. Explicit `on` entries override those defaults. `hold`, `unknown`, `timeout`, `invalid`, `cap`, `capacity`, and `deadline` are reserved outcome words; `on.hold` is a dead route, so it refuses.

The bare-`ci` commit fence sets `readings.ci` to null. When that makes the answer `unknown`, `reason` includes one of these, using the first seven characters of each commit OID (or all of a shorter OID):

- `ci (local HEAD <l7> is not on the PR, PR head <p7>)`
- `ci (read at <c7>, PR head <p7>)`

The local-HEAD reason takes precedence over the CI-commit reason; the pre-entry-probe reason takes precedence over both. The same text follows `unknown: ` in the waiting line and JSON `detail`, and remains in the journaled evaluation answer printed by `show` history.

`rimz loop list --json` carries `workflow_runs` for a workflow task: one object per active or parked run the task started in that room, `{id, address, checkout, state, node, since}` plus `reason` and `text` on a parked run. Both list and show add the shared `detail` when nonempty. `since` stays node entry; a human parked summary's age starts at the park.

`rimz loop show --json` lists, unfolded and unlimited, every run named by the task's launch memory first (a condition workflow task's or a `stay` task's), including terminal runs whose task was renamed, then every other run the task started in that room, live ones first and finished ones newest first, each once. A finished run adds `terminal_at` (RFC 3339). A run canceled while parked keeps its recorded `reason` and `text` in JSON, without appearing as parked in human output. A run whose record is gone has only `id`, `checkout` (the launch entry's key), `state: "gone"`, and `since` (the launch time). Fan-out `worktrees[].run` repeats the matching `workflow_runs` object; project-scoped launches share one run across checkout rows. The human WORKTREES table names those runs, and RUNS lists only runs no worktree row names, folding adjacent finished ones ([`loop show`](./loop.md#loop-show)).

## Exit codes

| Code | When |
| --- | --- |
| `0` | Success, including `cancel` of an already-finished run, `show <WORKFLOW>` with an unavailable definition, and `validate` with warnings only. |
| `1` | A refusal or failure: an unknown or ambiguous run, a duplicate start, a terminal resume or restart, a validation error, an unknown node, an unloadable sealed definition in the human `show`, and `show` with no RUN outside a workflow. |
| `2` | A usage error: a clap parse error, a malformed `<workflow>#<lane>` address, `show <run> <NODE>` with a run address or id, `--history` with a workflow name, `--history` or a NODE together with `--json`, and a name that is both a deployed workflow and a loop task without a `workflow:` or `task:` selector. |
