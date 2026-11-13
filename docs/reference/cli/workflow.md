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

`show`, `resume`, `restart`, and `cancel` accept a run address `<workflow>#<lane>`, a workflow name, or a full run id (`wf_` and 32 hexadecimal digits). The lane is the basename of the recorded scope path, even after that checkout is removed. Root-checkout and project-scoped runs use `main` (`review#main`); the project directory's name and the project path do not name them. A requested `feat/x` also matches `feat-x`. A target with `#` but an empty workflow or lane, or with a second `#` in its lane, is a usage error (exit 2). When a target begins with `-`, put options before `--` and the target after it.

An address selects its live run; only `show` falls back to the newest finished run when none is live, so `resume`, `restart`, and `cancel` reach a finished run only by its id. Two live runs sharing an address refuse and list each id and checkout with a command for it. A workflow name in `show` draws its definition, then lists that workflow's live runs, never one run's detail; a known workflow with none ends with `no active or parked runs of <name>`. For `resume`, `restart`, and `cancel`, a workflow name prefers the caller's checkout or its project's project-scoped run, then the only live run, otherwise lists commands to choose one. Matching runs in the caller's room take precedence; without a local match, several rooms refuse with `--root` commands. Each command a refusal lists names its run by address, or by id where another candidate in the same room shares that address, and carries `--root` when the run is outside the current directory's room. The global `--root` chooses the caller's room. Run ids remain unambiguous across rooms.

## Run a workflow

`rimz workflow run <NAME|DIR> [-w <WORKTREE>] [--prompt <TEXT>] [--give-up <WHEN>] [--skip-capacity]`

| Argument or flag | Meaning |
| --- | --- |
| `<NAME\|DIR>` | A single path component is a name under `~/.rimz/workflows/`; anything else is a directory, which must exist inside `~/.rimz/workflows/` (otherwise `<NAME\|DIR>: directory must exist inside <home>`). `.` is the current directory. |
| `-w, --worktree <WORKTREE>` | Select an existing RimZ-owned worktree instead of the current checkout. It never creates one. A workflow with `scope = "project"` refuses it. |
| `--prompt <TEXT>` | Stored unchanged, appended to every agent step's prompt, and exposed as `prompt` in `context.json`. The value may begin with `-`; for a value spelled like a flag (`--help`), write `--prompt=<TEXT>`. |
| `--give-up <WHEN>` | The run's give-up instant. A capacity wait at an agent step without its own `admission.give-up` gives up at `<WHEN>` and follows `on.capacity`. `<WHEN>` is a timestamp (`2026-11-13T21:00:00Z`), a local date-time (`2026-11-13T21:00`), or a date (`2026-11-14`), the forms a workflow `deadline` takes; any other value exits 2 with those forms. An instant already past gives up at the first hold. |
| `--skip-capacity` | The run's agent steps launch past every capacity hold, workflow and step limits and the machine [start throttle](./loop.md#the-start-throttle) alike, and record the holds they passed. A usage window or a checkout occupant still holds them. `restart` and `resume` keep the run's choice. |

Before creating a run, the command validates the definition and refuses on any error, printing each as `<path>: <message>`. Warnings alone do not refuse; each prints to stderr as `warning: <path>: <reason>` before starting. It needs an existing room (`rimz start` creates one). With `scope = "worktree"`, nodes run in the selected checkout; with `scope = "project"`, they run at the project root, even when invoked from an owned worktree. One active or parked run holds each `(workflow, scope path)` pair, and a run a loop task started also holds `(task, scope path)`; a second start refuses and names the existing run. The CLI validates before it asks for that slot, so a broken definition still refuses on validation first. On success it prints the run id, then the run's work directory (`RIMZ_WORKFLOW_DIR`), then `→ rimz workflow show <run>`. The first two lines keep their scripting format. The first advance starts in the background, and the command waits up to 30 seconds for its first decision: when the run is held in front of its node by capacity, a usage window, or a checkout occupant, a fourth line names the node and the hold in the phrase `show` prints, such as `build held 0s · claude: no capacity (your check)`. A run that launched, ended, or whose advance exited without deciding prints the three lines alone. The work directory includes `notes/`, a link to the run's kept notes folder, and `signs/` for step guidance. Every evaluation and attempt takes a new `seq`, a retry included, so name a notes write after the item it records to make a repeat overwrite it.

Before it writes anything, `run` opens the home-wide start lock, `~/.rimz/loops/throttle.lock`, for write without taking it, since the background engine opens the same file on its first launch; `resume` and `restart`, including `rimz agents restart` on a workflow agent, do the same. When the open fails, the command exits 1 with `nothing was started` (`resumed`, `restarted`) and the cause, and leaves no run, record, or journal change behind. A read-only RimZ home, for example inside a provider's command sandbox, adds `this command cannot write the RimZ home` and the [sandbox remedy](./wait.md#arm-a-wait), worded for `rimz workflow`. The check covers RimZ's own files only: a check script that writes elsewhere, such as a tool's cache, can still fail inside a sandbox.

`-w` on a project-scoped workflow refuses before anything is created, with `<workflow> has scope = project, so it runs at <project root>; start it without -w` (exit 1). A loop task's fire into a worktree is not a `-w` start and keeps running at the project root.

### Failures

A node fails with one of eight causes, and each kind raises only its own:

| Kind | Causes |
| --- | --- |
| `agent` | `died`, `timeout`, `limited`, `visits`, `capacity`, `deadline` |
| `command` | `died`, `timeout`, `visits`, `deadline` |
| `message` | `undelivered`, `visits`, `deadline` |
| `test` | `died`, `timeout`, `deadline` |
| `script` | `died`, `invalid`, `timeout`, `deadline` |
| `judge` | `died`, `invalid`, `timeout`, `limited`, `deadline` |
| `condition` | `timeout`, `deadline` |
| `sleep` | `deadline` |

| Cause | Raised when |
| --- | --- |
| `died` | The node's process could not start or ended abnormally: an agent launch that failed, including a failed continued restart; a turn step's provider exit before its clean turn end; a session's exit away from rest ([steps and checks](#steps-and-checks)); an agent attempt a room rebirth ended; a command's or script's nonzero exit or signal; a test killed by a signal; a command, test, or script that could not spawn or open its log; a command whose host died; a judge process that failed or wrote no JSON answer. |
| `invalid` | A script or judge gave an answer the node cannot use: not one JSON object, a wrong field type, an outcome its `on` does not name or a reserved word, or a bad `recheck_at`. |
| `timeout` | The node's `timeout` passed. |
| `limited` | A budget refused an agent step's launch or stopped its run, or a judge's account may not spend (the fleet's or the account's daily cap is spent, or the account is parked). |
| `undelivered` | A message step found no single living agent, its prompt could not be composed, or the address resolved to another session than its pinned `session`. |
| `visits` | An entry would exceed the step's `max-visits` plus the run's visit grant. Nothing launches and no visit is counted. |
| `capacity` | An agent step's capacity wait gave up, or its start can never fit under a cap. No visit is counted. |
| `deadline` | The run's deadline passed while the run was at the node. A live attempt or evaluation stops first. |

A failure takes the node's `on.<cause>` when set, else the node's `on.failed`, else `[workflow] on.failed`, else the run ends `failed`. A failure never follows `then` or an answer route, and a failure route may not be `@wait` (`a failure route cannot wait; route it to a node, @done, or @failed`). Validation refuses a `visits` or `capacity` route that, after this chain, can reach its node again, since those failures count no visit: `<path>: <workflow>: the visits route out of <node> leads back to <node>; it counts no visit, so route it where the run cannot return`, at the route that applies (`<kind>.<node>.on.<cause>`, `<kind>.<node>.on.failed`, or `workflow.on.failed`).

The deadline fails the node the run is at once per grant: the node it routes to runs without a deadline until `resume --extend deadline=<WHEN>` grants another. A judge that fails with `died` or `invalid` runs once more, with `retry` in `context.json`, before its failure is raised; a judge's timeout or `limited` fails at once, and a provider limit mid-run waits for its reset without failing. A fresh agent launch whose takeover could not stop the checkout's occupants, before it started anything, is tried once more: the step waits as [`retry`](#the-waiting-line) with no attempt or visit counted, the run's next advance launches again, and only a second failure is `died`. Each new entry into the step gets its retry back.

A condition's reading error and a sleep's evaluation error are not failures of the node: the run ends `failed` with the error as its reason and takes no route. A blocking question on a turn agent step takes `on.asked` when set and otherwise stays open in the agent's pane; it is not a failure. A `workflow cancel` and a `workflow restart` take no failure route.

`@break` and `@user` refuse as targets anywhere (`<path>: @break is not a route target; route to a node, @done, or @failed`).

### The failure input

Every failure route hands its target this object as `input`, with the run's `key` beside it when the run has one:

```json
{ "outcome": "failed", "cause": "visits", "node": "fix", "reason": "fix has used 3 of 3 visits", "detail": { "visits": 3, "max": 3 } }
```

`node` is the node that failed and `reason` is always a string. `detail` is always an object, holding only its cause's members:

| Cause | `detail` members |
| --- | --- |
| `died`, `timeout` on a command step | `code`, the exit code (absent after a signal or a spawn error), and `log`, the step log's path when it exists |
| `died` or `invalid` on a test or script once its `.check.log` exists | `log`, the `.check.log` path, and `code` once its process exited (absent after a signal, and when the process could not spawn) |
| `limited` on an agent step | `reset_at`, when the provider's reset is known |
| `capacity` | `waited_ms`, how long the start was held, and `first`, when the hold began; `never_fits: true` when the start is wider than a machine cap |
| `visits` | `visits`, the step's count, and `max`, its limit with the run's grant |
| `deadline` | `deadline`, the instant that passed |
| any other | none |

The attempt the failure route admits records the cause as its `routed_by`. `show --history` prints the route as `route <cause> → <target> · <fields>`, each of `reason`, `key`, and the `detail` members as `name=value`, joined by ` · `.

### Steps and checks

The step leader's Workflow launch note describes its listed checks in order. Scripts and shells show their command and optional summary, explain their execution/path rules, and say the agent may run them itself. A judge shows only its summary, never its prompt filename; a sealed judge without one gets a fixed agent-judge description.

`[workflow] start` must name a check or a step, not a special target such as `@done` or `@user`. A step at the start runs through the same admission as any later visit. Its first `context.json` has `input`, `key`, and `prev_attempt` set to null, with `visit` and `visits` both 1.

Workflow agents, judges, tests, scripts, and commands receive `RIMZ_WORKFLOW_RUN` (run id), `RIMZ_WORKFLOW_DIR` (work directory), and `RIMZ_WORKFLOW_DEFINITION` (the absolute folder of the run's sealed definition, `definition/` in its run home). Every workflow agent gets an Environment `- workflow:` line even with `env_reminder` off. Non-workflow agent launches unset all three variables. If the agent launch cannot read the run record, it warns and omits the definition variable and line.

Every work node requires `then`: for an agent one check, `@done`, or `@failed`, for a command one check, for a message exactly one node, `@done`, or `@failed`. An agent's `then` is where the run goes when the attempt ends on its own; `@done` or `@failed` ends the run there. A list refuses at `agent.<step>.then`. A command requires `timeout`. On a `command`, `test`, or `script`, `run` is a literally passed argv, with slash-containing words confined to the definition directory, and `shell` runs through `sh -c` without path confinement; which one a node sets decides only how its command starts. Each kind takes only the fields its [table](#definition-fields) lists, so a command never waits for checkout occupancy or capacity. Their stdin is closed, both streams go to `work/<seq>.step.log`, and they inherit the workspace pin and workflow environment.

A message step's `message` is an agent address, `@<handle>` or `@<handle>#<channel>`; `@user`, `@me`, a bare name, and a pane id refuse at `message.<name>.message`. `prompt-file` is optional and confined. A message never reads occupancy, windows, capacity, or the start throttle. Each visit composes a header line naming the workflow, step, and run, followed by the prompt file, signs, and run prompt, as an agent step's prompt is composed; it resolves the address from the run's checkout among living agents and queues one automated `Type: WAIT` message from `@rimz` at gate `done`, sent at once to a resting agent. It records `ended` with the target's handle and follows `then`. No single match, or a prompt that cannot be composed, records `died` with the reason and fails the step with cause `undelivered`. `session = "<agent session id>"` pins the step to one session: when the address resolves to an agent on another session (a handle since reused), the visit records `died` with `<address> is session <id>, not the pinned session <pin>` and fails with cause `undelivered`. A lost host sends the same message id again on the next advance, and the queue keeps one copy and its first target. A cancel, deadline, or bound that ends the attempt before it settles retires the message if it is still queued; a resting target may already have received it.

An agent step requires `prompt-file` and `lifecycle`, `turn` or `session`; a missing `lifecycle`, or any other value, refuses at `agent.<step>.lifecycle`. Its `agent` accepts the fresh-launch layout grammar, including agent and command cells. A `turn` step ends at the leader's first clean turn end. A `session` keeps the attempt live across turns until the leader's provider process exits, and an exit at rest follows `then`; with a `rest-check`, described below, its clean rests are also judged. `timeout`, `keep`, and `on.asked` apply on both lifecycles. A step's `timeout` bounds its attempt from launch, less the time spent held on blocking asks, provider limits, errors, and budget parks; while a session is held at a rest check, that check's own timeout applies. The timeout ends the attempt as `timeout` and fails the step with cause `timeout`. A session's `subscribe` takes `wake` entries, with the same defaults read for the checkout its leader launched in (its `worktree` when set), and also the team lifecycle signals (`team.idle`, `team.waiting`, `team.failed`, `team.ended`), which default to the team's cohort in that checkout, and `agent.*` selectors whose `match` names another agent by `handle` or `session`; the leader's own `agent.*` signals never reach it. It queues each matching signal for its leader as a `Type: SIGNAL` message from `@rimz` at the leader's next turn boundary, sent at once when it rests. The message opens with the signal's subject and details, then names the workflow, step and run. A table entry may add `prompt = "..."`, which follows after a blank line in each message that entry delivers. Each entry that matches sends its own message; entries with the same signal and `match` send one. A forward changes no run state, and a check subscribed to the same signal still wakes. Forwarding stops when the attempt ends, and a forward still queued then is canceled at delivery. A `turn` step refuses `subscribe` with `only a session receives signals; a turn ends where they would land`. A session without a `rest-check` may name a team: it launches the team's cohort the way a `stay` loop task does, its leader as the prompt leader. A team on a `turn` step, or on a session with a `rest-check`, refuses with `a team runs only in a session with no rest-check; set lifecycle = "session" and remove rest-check`. Command-only layouts and ambiguous prompt leaders refuse with a field-specific fix. Judge checks still take one profile. An agent step's `admission.window` gates every agent cell's provider.

An agent step's launch settings apply to each fresh launch as the matching `rimz agents` flags would: `account` (a login name, pinned on every cell; capacity admission counts the launch on that account and skips availability routing), `mode` (`auto`, `ask`, or `yolo`; `plan` refuses), `effort`, `fast` (true or false), `budget` (the `--budget` grammar, such as `"$5"` or `"$20/day"`), `system-prompt-file` (a file confined to the definition directory, refused at `agent.<step>.system-prompt-file` otherwise), and `worktree` (a worktree name the launch creates or reuses, empty for a generated name; the agent then starts there instead of the run's checkout). A restart that resumes a session keeps its account, system prompt, and checkout, and applies the rest. Only a clean end follows `then`; every other ending is a [failure](#failures).

`rest-check = ["tests", "review"]` on a session names the checks that judge its clean rests. Each entry must name a test, script, judge, or condition. An empty list refuses with `rest-check must list at least one check`; a step, a sleep, an `@` target, or an unknown name with `rest-check lists only check nodes (test, script, judge, condition); <entry> is not one`; and `rest-check` on a turn step with `a turn is judged at its turn end; rest-check is a session's field`. At each clean leader turn end, with the leader at rest and no ask or stop open, the run holds the session and enters the first listed check. An answer routed back to the step becomes the next prompt in that session, updates input/key and spends one visit; a push-back past the cap fails the step with cause `visits` instead, closing the agent by default. A held `@wait` spends no visit and rechecks on subscribed signals and the minute tick while the leader rests. A route to another listed check, a failure route included, keeps the session open, resets check retry and starts the destination check's clock; an answer keeps the step's input/key, and a failure hands the next check its failure input. Any other route closes the session and goes there, closing the layout by default. A new turn abandons a held wait and any check in flight; the next eligible turn end starts again at the first check. Provider exit runs no rest check: an exit at rest follows `then`, and any other is `died`. Each listed check is an edge of the step with outcome `rest-check`, so `validate` lists it, a rest check that routes back to its session is a cycle that needs `max-visits`, and a listed check is never unreachable. A listed check reached without a held session routes normally.

Failed or interrupted turns, live provider limits, and budget parks keep every workflow agent step open. A turn step finishes on a later clean end; a rest check judges only a cleanly rested turn. Providers that report an interrupt as a clean end still let the check judge. A step's timeout pauses during blocking asks, limits, errors, and budget parks, counting overlaps once; silence and interrupts do not pause it. Deadlines never pause. A turn step's provider exit before its clean turn end is `died`. A session's leader exit is `ended`, and follows `then`, only when it had ended a turn and was at rest: no turn open, no blocking ask, no errored turn end, no background work or pending `rimz wait`, and no budget park on its card. A message still waiting for the leader, or a child result it has not collected, does not count against an exit at rest. Any other registered exit, and an exit before registration, is `died`. A budget refusal before launch is `limited`.

`keep = true` on an agent step of either lifecycle leaves the leader and companions open, outside the workflow, on a natural attempt end, an `on.asked` ending, or a closing rest-check route, a rest check's failure route included. Default false retains cleanup. A forced ending (cancel, restart, `resume --at`, the step's `timeout`, the deadline) and a `died`, `limited`, or `canceled` attempt still stop them; a push-back past `max-visits` detaches them. A `turn` check returning to the step can leave the old agent beside the new one; occupancy still applies.

Agent steps default to notes on. Their launch brief names `<step>-notes.md` in the run's kept notes folder as working memory across visits, restarts, and subagents, including a record of changes outside the repository. `notes = false` omits that instruction; only an agent takes `notes`. New runs link `work/notes` to their dated folder under the room's `automation-notes/`; `gc.automation_notes_retention` defaults to keeping closed folders forever, independently of run-home cleanup.

A check's `on.<outcome>` is a target string: a node, `@done`, `@failed`, or `@wait`. A table or any other value refuses.

A sleep takes required `sleep` (nonzero `s`/`m`/`h`/`d` duration, at most `365d`) and `then` (node, `@done`, or `@failed`, never `@wait`), optional `when` (a nonempty ANDed loop-condition array) and `for` (a continuous hold shorter than `sleep`). It may take `provider` and `account` for window terms. Expiry follows `then` even during a hold; a true condition follows it early. Unread data, including under negation, never wakes it. Re-entry or a parked resume restarts its clock. A sleep that completes forwards its incoming input and key; its one failure, `deadline`, hands its route the [failure input](#the-failure-input). An error evaluating `when` ends the run `failed` with no route. A sleep needs no timeout. A `rest-check` cannot name a sleep.

### Limits and capacity

`[workflow.limit]` limits the agent steps that runs of that workflow name have going across rooms, and refuses with `set max-parallel or stagger` when it has neither field below. An agent's `[agent.<name>.admission]` takes the same two fields and limits that step's starts. An unknown key or an invalid value refuses:

| Field | Rule |
| --- | --- |
| `max-parallel` | Integer at least 1. Counts one per unfinished run whose record holds an agent step attempt, including an idle session, a session held at its rest check (counted under its own step), and an attempt whose agent has not registered yet or has died, until an advance settles it. Companions, RimZ-launched children and peers, headless judges, and a kept agent whose attempt has ended do not count. A step with companions counts as one, so a layout wider than the limit still starts. |
| `stagger` | Positive duration in `s`, `m`, `h`, or `d`, at most `365d`. Minimum gap between launches at this scope, stamped at launch and shared across rooms and restarts. |

Limits use each run's sealed definition and count runs of that workflow name, so a finished run frees its slot even when it kept its agents open. Waiting, paused, hibernated, and compacting sessions count while their run holds the attempt, and so does a parked run's. The incoming start counts as one, and a same-workflow start admitted but not yet launched holds the next one until its launch is reported. Limits apply after window and occupancy, before the two machine caps and other machine checks, even with no `[throttle]` table. No workflow or step limit bypasses a machine cap. Earlier Active capacity waiters held by a workflow limit go first across all steps; a step-limit waiter holds only its step. Ties use run id. A freed slot waits for the earlier run's next retry. Limits never stop an existing turn or pane.

Agent steps also ask the [start throttle](./loop.md#the-start-throttle) before taking over a checkout, whatever started the run: `workflow run`, `loop fire`, a scheduled fire, or a `restart`. `admission.throttle = "off"` skips the machine turn, stagger, resource limits, and capacity check, not the machine caps or applicable workflow and step limits. `admission.give-up` accepts a positive `s`, `m`, `h`, or `d` duration, measured from the first hold, or `forever`. Without it, a held step gives up at the run's give-up instant (`--give-up`, or the one a scheduled loop fire sets for a task that is not a deployed workflow: just before a repeating clock trigger's next fire, or the task's deadline when that is earlier), and with neither it waits until the run is canceled. A held step spends no visit and retries about once a minute. Giving up fails the step with cause `capacity`, for machine and workflow limits alike; its route, after the [chain](#failures), may not lead back to the step. A window or occupancy wait restarts the capacity clock. The run deadline still applies. A run started with `--skip-capacity` launches past each hold instead: it claims any applicable workflow limit, counts as automated afterwards, restarts machine, workflow, and step stagger, and records the holds it passed. Checks, command steps, and already-open sessions never ask for admission.

Limit holds are self-clearing (`workflow_limit` or `step_limit`), shown last in their workflow group. Reasons start with `workflow <name>: ` or `step <name>: `, followed by `N of M agents open (@a, @b, +K)`, `next start in <duration>`, `N earlier starts waiting` (singular `1 earlier start waiting`), or `N starts launching` (singular `1 start launching`) while a launch of the same workflow is not yet counted. Names list at most two handles, then `+K` for the rest; the suffix is omitted when empty.

## Inspect definitions

`rimz workflow show <WORKFLOW>` draws the current definition in walk order, with returning steps hung beneath their earlier sending check. Checks draw `◇`, steps `○`. `→` points to a later node and `↑` to an earlier node or itself. Unreached nodes follow under `unreachable from <start>`. The header names the summary, scope, loop tasks that start this workflow in the caller's project (machine tasks only outside a room), any workflow `limit`, and any configured deadline route. Without a loop task it gives a `rimz loop add` command and the by-hand `rimz workflow run` command.

Each node shows its runner, summary when set, routes, and routing limits. Condition cases retain their order and hold durations, followed by `none true`; the note names where an unread term goes. A message step's runner is its target address. Timeouts, visit caps, windows, `takes over` or `shares the checkout`, bounded `give-up` waits, and the lifecycle (`1 turn` or `session`) stay beside their nodes. Edges that cannot fire are omitted, and so is each failure edge left to its default `@failed`, a step's `capacity` edge included; an explicit `on.capacity` draws it. Uniform turn-end, invalid-check and unset-deadline defaults are stated once in the footer, only when used. `validate` and the node view show the omitted edges with their marks. The footer names the source file and a command for one node in full. The live-runs table follows the definition.

A missing or unloadable definition prints `<workflow>: definition unavailable: <error>; check with rimz workflow validate <workflow>`, then still lists its runs and exits 0. An unknown workflow with neither a definition directory nor runs refuses. `show <WORKFLOW> --json` keeps its live-runs array and does not include the rail.

`<WORKFLOW>` also names a loop task in the caller's project that compiles to an inline graph (every task except an unguarded `--wait` and a `workflow =` task). `show` draws the compiled graph with the task's prompt and judge files materialized as assets, the footer reads `compiled from loop task <task>`, and the live-runs table lists that task's runs. A name that is both a deployed workflow and such a task refuses (exit 2) and names both selectors: `workflow:<name>` picks the deployed workflow and `task:<name>` the task, in `show`, `show <WORKFLOW> <NODE>`, and `validate`. `task:<name>` for a task that compiles to no inline graph refuses with `no loop task <name> compiles to a workflow` (exit 1).

`rimz workflow show <WORKFLOW> <NODE>` prints one definition node in full: runner kind and summary, predecessors with the outcomes that reach it, check subscriptions (derived for conditions, explicit otherwise) and the minute tick, a step's `receives` subscriptions with their resolved scope (`for this checkout`, `for this checkout's team board`, `where k=v`, or `anywhere in the room`), ordered condition cases with holds, every resolved edge with its source and fireability mark, then the configured command text, message target, agent layout, prompt path, lifecycle (`turn` or `session`), `rest-check` (the listed checks joined by `, `), occupancy, keep, window, throttle, limit, give-up, visit bounds, timeout, provider and account. A limit reads `max-parallel=N · stagger=<duration>`, omitting unset fields; the definition header prints its workflow limit in the same form. Commands keep their full text, including multiple lines. Never-firing edges and a default `capacity` edge appear here even though the rail omits them. Predecessors omit never-firing routes; conditional ones carry the shared mark.

The two-positional form takes a workflow name only, not an address or run id (usage error, exit 2), except that a finished run's id or address refuses with exit 1, saying the run is finished and naming the `rimz workflow show <run>` that prints its saved history; it conflicts with `--json` and `--history` in clap. An unknown node refuses with the definition's node names (exit 1). A missing or unloadable definition refuses with its validation command (exit 1); there is no runs-table fallback for a node view.

## Inspect runs

`rimz workflow show [RUN] [--history | --json]` defaults to the calling agent's workflow run, including from its subagents. Outside a workflow, an omitted RUN refuses with exit 1 and points at `rimz workflow show <run>` and `rimz workflow list`. The node view still requires an explicit workflow name. When a live run's sealed definition cannot load, the human view and `--history` refuse with the load error (exit 1), and `--json` still prints the record and journal. A finished run never reads its definition: `show` prints `<address>`, its end state, its reason when the record names one, and `run <run>; the run is finished`, then the rows from the saved `history.txt`, omitting that file's opening name, end state and reason so each prints once; `--history` prints that saved text alone; and `--json` prints `address`, `dir`, the raw `record`, `journal`, and the saved text as `history`, whatever record version wrote it. Paths inside a finished record and journal name the run directory it had while live.

The compact view names the absolute kept notes folder after the work directory when the run has one. When a run ends Done, Failed, or Canceled, RimZ writes the colorless `--history` view to `history.txt` in the run directory and atomically copies it to the notes folder's `history.txt`, then moves the run directory to the room's finished runs. Terminal history uses the recorded end in the headline and date-qualified local clocks (`YYYY-MM-DD HH:MM`) on every row, so a later view or GC backfill reproduces the saved text; compact views continue to age. A failed copy or move warns without failing the committed transition and leaves the run directory in place; the next `advance`, `cancel`, or `restart` of that run retries. GC backfills a missing notes copy from the run directory before removing a finished home and keeps the home if copying fails. Runs without notes write no copy.

History prints prompt and script digests as their first 12 hex characters and omits empty stopped/live child groups. When unread fields share one cause, their reason lists the fields and names the cause once. JSON retains full digests and recorded reasons.

When a step of a `--skip-capacity` run launches past a hold, `show` prints `<holds>; started anyway: the run skips capacity` for its current session, including while held at a rest check. `--history` prints it inside that attempt's block after settlement. The holds are sampled at the admission claim, for restarts and later steps too, not at `workflow run`, which prints no such line. They may name an in-flight launch, an earlier waiter, a full cap, a stagger gap, a machine hold, or a capacity check that declined or failed, joined by `; `. A start that met no hold adds no note. JSON carries the text in optional `started_past_limit` on the live attempt and the journal attempt; absent values are omitted. A run that waits never carries it.


`rimz workflow list [--json]` prints every active and parked run across rooms, without column headers. NEEDS YOU holds parked runs, active runs with a blocking ask, and capacity holds needing a fix (`check_failed`, `disk`, `store`), longest waiting first, with full addresses and a `workflow show` hint for its first run. The rest group by workflow name, each heading reading `<workflow> · N active[ · M held]`. Self-clearing capacity holds sort last in their group; each part sorts by address. NEEDS YOU rows are `<glyph> <address>  <detail>  <age>`: `?` for an agent's blocking ask, `⊘` for a capacity hold needing a fix. The latter uses the `workflow.held` glyph in both sets, also in the `show` headline and current rail node; it is not an agent question or a paused agent. Grouped rows are `<glyph> #<lane>  <detail>  <age>`: `◉` for self-clearing capacity holds, `⢿` for with-you sessions and active work, `☾` for other active waits, from the configured glyph set. A group names `started by <origin>` when all its runs share the loop task and manual-fire flag; otherwise each row adds ` · by <origin>`. An origin is `hand` or `loop task <task>[, fired by hand]`. Age starts at the park, at the opening of an ask, at the first capacity hold, or at node entry otherwise. Detail truncates before age at narrow terminal widths. Each row whose sealed definition loads is followed by the horizontal rail described below. Several rooms get headings (the home-relative project root), the caller's room first, and hints for other rooms carry `--root`. Finished runs are omitted. With no active or parked runs, it prints `no active or parked runs; start one with rimz workflow run <name>`, and `--json` prints `[]`.


Detail is `asking you: <handle> (<kind>): <one-line text>` for a blocking ask; otherwise a live session reads `with you in @<handle>`, with ` · checking <check>` while held and `: <wait phrase>` at that check, or ` · <check>: <wait phrase>` after it when waiting. The handle matches its agent card, without the channel already named by the row; if unavailable, it falls back to the recorded pet name. A capacity hold reads `held · <reason>`; other runs print the [wait phrase](#the-waiting-line), `running` (with ` · visit <n> of <max>` when capped), or `checking`. A cached PR number adds ` · PR #N` once per row; no cached number means no badge. Each room supplies its own agent names and PR cache. JSON reads neither that enrichment nor the journal: its keys and shape stay unchanged, and `detail` retains `with you` without an agent or PR and `held <age>[, gives up <clock>] · <reason>` for capacity waits, without node or the separate age column. A single-run `show` says `<step> is asking you` while asking, otherwise `<step> is with you` for a live session attempt. Its session attempt is `held.attempt` while checking, otherwise `attempt`; its `lifecycle` is `session`, omitted for turn attempts. An open blocking ask is stored in `asking`; `asked_ms` is the total closed pause time in milliseconds. Both are omitted when empty or zero. While held, the rail marks both the session step and its check current.

`rimz workflow show <RUN> [--history | --json]` opens with `<address>[  PR #N]`, then the activity glyph and current state on a second line. Only the state phrase takes the state tone; agent context uses body tone and the bound forecast is muted. A live agent uses its full card handle, including the channel, with the recorded pet name as fallback; `--history` keeps pet names. A with-you headline adds the same check/wait suffix as the list, without repeating it below. A wait names its node, elapsed time, recheck and the next bound's time and target; a hold names its outcome, duration and the next bound's time and target. The forecast picks the earliest unfired deadline, timeout or hold completion, in that order on ties. A running step shows its visit limit and agent handle when available. Asking reads `<step> is asking you · <handle> (<kind>)` and says to answer in the pane or with `rimz answer`; the step's timeout pauses while asking. A parked headline reads `parked at <node> <age> · <reason>`, leaving its explanation to the block below. Finished runs say `done`, `failed`, or `canceled` and show no PR badge. Their compact headline ages from termination; terminal `--history` uses the ending instant as its clock so its saved copy stays reproducible. The origin line names when and how the run started (by hand or by loop task), the number of completed attempts, and their known total cost, including judge evaluations. It omits cost when none is recorded, and never estimates a live attempt's spend.

On an active run, the current check's latest evaluation since entry supplies one saved readings line below the headline: `key=value` pairs separated by two spaces, null as `key=unread`, then ` · read <clock>`. Script and condition readings use the same line, with no per-case value annotations.

Alongside the saved readings, a human live or parked run shows one `now <check> · <outcome>` line for its condition check or sleep with `when`. On an agent step it reads the first rest check, else the `then` check; on a parked check it reads that check. The line uses current `key=value` readings, null as `key=unread`, and the evaluator's unread reasons. A winning case with `for` shows the remaining hold from its open wait for that case, or the full duration without one. Forge keys add ` · read <clock>` for the cached probe; board-only checks omit it. The freshness floor is the step or parked node entry, or the held session entry while held. It launches no check, writes nothing and refreshes no probes; the CI fence may make read-only Git calls. Script, shell, judge and plain-sleep checks have no now line. JSON and history are unchanged.

The block below the origin shows the full ask text when asking. A takeover wait adds `takeover:` lines naming the agents that hold the scope path (`blockers unavailable` when the agent roster or the project's worktrees cannot be read). The `you can` commands a parked run prints, history footers, receipt hints and NEEDS YOU hints use the address when it selects this run, otherwise the run id, and add `--root` for another room. A timeout shows the flushed wait as `<node> said <outcome>` with a folded count and span when present, and the last shell-check log; `invalid` shows the recorded stderr tail.

Active runs paint the definition rail after the block below the origin and before `visits`, using the run's sealed definition, never the edited source. Reached steps draw `●`, reached checks `◆`, with green glyphs and body-tone names. Unreached steps draw `○`, checks `◇`, in muted tone. Current nodes use the headline's glyph, bold in the state tone, including both the session step and check while held. Reached nodes add their last check outcome or completed attempt (outcome, duration, known cost, recorded HEAD before and after), its local clock, and `×N` for repeat entries. A capped step adds `visit <used> of <max>`, including the run's grant. Equal recorded commits read `HEAD <a>, no new commits`; different commits keep `HEAD <a> → <b>`. The current check names its outcome, `since <clock>`, `last checked <clock>` from its latest evaluation, wake selectors and the minute tick, with no advance count. A current session adds `receives` and its subscribed selectors. False cases are faint; the winning or held case reads `│ ▸ N`, bold blue except green for `@done` and alarm for `@failed`. The first unread case keeps `?` and `<key> unread: <reason>`. Later cases are unmarked and muted, except that `@done` and `@failed` cases keep their definition tone. A held case shows its elapsed and required duration and target, with a completion clock only when that completion is the next bound. Marks remain visible with `NO_COLOR=1`.

Parked runs put a horizontal rail after `you can`; finished runs also use it, with no current node. Steps draw `●` when reached and `○` otherwise; checks draw `◆` and `◇`. The current node uses the headline's activity glyph. Reached glyphs are green with body-tone names, current nodes are bold in the state tone, and unreached nodes are muted. A finished run's ending node is where it stopped: alarm-colored for failed runs, green for done or canceled runs. With colour off, the ending node has no mark of its own and draws as any other reached node, `●` or `◆`. Nodes join with ` ─ ` in walk order. A visited, current, or terminal-ending detour hangs below its sending check as `╰─ <glyph> <step>[ ×N] ↑`, with the visit total above one. Unreachable nodes appear after the main rail only when current or reached, including after `resume --at`. To fit a terminal, the rail drops nodes from the left as `… `, then from the right, regardless of whether they were reached, always keeping the current or ending node. Next, nodes between a held run's two current nodes drop as ` ─ … ─ `, and then the predecessor, which stays only while it fits. If that still does not fit, visited detours under the kept nodes drop, except those that are current or ending, and dropped nodes return wherever they fit. A detour drops with its parent unless it is current or the ending, which keeps its parent visible. Names are never shortened, so a current or ending node or detour wider than the terminal is drawn whole. Piped rails are not shortened. Capped visit totals remain in the `visits` rows below.

`visits  <step> <used> of <max>` lists each visited capped step, with the run's grant included in the limit, counted since its last manual restart. Uncapped steps print no visit rows. The `trail` projects routing decisions from the sealed specification and journal, folding consecutive matching check answers as `×N`. It includes resumes, restarts, parks and termination, keeps at most the newest eight hops with `+N earlier` for the rest, and ends in `now` while live. The footer names the home-relative work directory and the command for full history.

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
| `held <age>[, gives up <clock>]` | An agent step is held by machine capacity, a workflow or step limit, or a start queue still busy after 30 seconds. Its give-up time is absent for `forever` and when neither the node's `admission.give-up` nor the run's give-up instant bounds the wait; the reason follows ` · `. |
| `recheck` | The check's `recheck_at`, before which nothing evaluates; a signal matching that check's subscription clears it. For a `for` hold it is when the hold completes, and subscribed signals and the minute tick re-sample the case before then. |

## Cancel and resume

`rimz workflow restart <RUN> [--continue]` replaces the current agent step, including a session held at its rest check or a run parked at an agent step. It stops the owned layout, keeps input and key, resets that step's visit count, and starts at visit 1. Its relaunch waits for the step's usage window, checkout occupancy, and capacity as any start does, and a layout wider than a machine cap raises `capacity`. Other steps' counts and run-wide grants are unchanged.

Restart cancels any unconsumed push-back from the abandoned attempt, including one already sent to the pane, so it cannot delay judgment of the replacement's next clean turn. Other messages are unchanged.

Without `--continue`, the replacement starts fresh. With it, the leader resumes the current attempt's recorded conversation under the room's current account; from a parked agent step, it uses the previous attempt only when that attempt belongs to the same step. A still-live previous card is stopped before relaunch, and companions start fresh. A missing conversation, unsupported provider resume, or a different account history pool refuses before anything stops, naming a fresh `workflow restart` or `rimz accounts use` fix. If resume preparation fails after the stop, the step fails as `died`. These refuse without stopping anything: a finished run (`workflow run <id> is <state>; cannot restart`), a run being canceled, a check without a held session, a command or message step (`node <node> is <kind>[; @user: <park> | ; waiting on <outcome>]; restart requires an agent step`), and a restart already pending (`this step is already restarting`).

The command waits up to 30 seconds for the replacement to launch, park, or end the run, then prints one receipt line and a `workflow show` hint, followed by the held line `run` prints when the replacement waits in front of the step. The mode is `fresh` or `resumed (<session>)`.

| Outcome | Receipt |
| --- | --- |
| Launched | `restarted <address> step <step> as <handle> · <mode> · visits restarted at 1` |
| Parked | `restart <address> step <step> · <mode> · no live replacement: <park>` |
| Run ended | `restart <address> step <step> · <mode> · run is <state>; no live replacement` |
| Still pending (an occupancy wait, a stop in progress, or 30 seconds passed) | `restarting <address> step <step> · <mode> · awaiting launch; visits reset` |

`workflow show` follows a pending restart's progress. History records the canceled live attempt, a full visit reset, and `<step> restart · fresh|continued · visits restarted at 1`; stopping a previous attempt again does not repeat its end or cost.

The journal records `{event: "restart", step, continued}` and `{event: "visits_reset", step, outcome: "restart"}`.

`rimz workflow cancel <RUN>` ends a run as `canceled` and stops its live script, shell, judge, or step attempt. It prints `canceled <address>`. An already-finished run id prints `<address> is already <state>; unchanged` and still exits 0. Both receipts end with a `workflow show` hint.

A run that ends moves from the room's live run directory to its finished one, so a finished run id still resolves and `show` prints it, from any room, without its definition: see [Inspect runs](#inspect-runs). A run whose record a newer RimZ wrote is left for that RimZ: `workflow list` skips it, and `show`, `resume`, `restart`, and `cancel` by id refuse with `` workflow run <run> has record version <N>, written by a newer RimZ; this RimZ reads only version 9. It is left for the RimZ that wrote it; nothing changed `` and exit 1. An unfinished run this RimZ cannot continue (an older record version, a missing or invalid one, or a version-9 record that does not decode) is skipped by `workflow list` too; `show` prints its header with why, and `resume` and `restart` refuse with `` this RimZ cannot continue workflow run <run>: <cause>; it reads only record version 9. The workflow pass will end it as failed; nothing changed ``. A `record.json` that holds no JSON object refuses the same way, ending `The workflow pass will set it aside; nothing changed`. `cancel` and `advance` on such an id end that one run instead: its record is marked `failed` with why, in place, its agents are released and left running as ordinary sessions, its loop task gets a `failed` report that strikes nothing, and its home moves to the finished directory; a record that holds no JSON object moves there unmodified. `cancel` prints `` ended <run> as failed: this RimZ cannot continue it: <cause> `` (or `` set aside <run>: its record holds no JSON object ``), then the agents it left running or `no agent was pinned to it`, and a `workflow show` hint; it ends no other run. When an earlier `cancel` or pass already ended the run but could not release its agents or move its home, `cancel` and `advance` retry the report, the release and the move, and `cancel` prints `` <run> was already ended as failed: this RimZ cannot continue it: <cause> `` in place of the first line. The room's scheduler does the same for every such run on its own, at most once a minute: see [the workflow pass](../../internals/harness/workflow.md#the-workflow-pass). Background readers skip all three until then: the loop scheduler, the start limits' count of open workflow agents and worktree cleanup. `rimz gc` removes a finished run's home once its record is more than seven days old, whatever version wrote it, and never removes a live run's home that holds a record.

A cancellation journal row may carry `reason`; `show --history` prints `cancel canceled · <reason>` when present, otherwise `cancel canceled`.

An unfinished run holds its scoped worktree against removal, even while parked with no agent. `rimz worktree remove NAME --force` removes the checkout and then cancels its unfinished runs with reason `worktree removed`. A failed cancellation leaves the removal standing, exits non-zero, and prints the run id's `rimz workflow cancel` command to finish it.

`rimz workflow resume <RUN> [--at <NODE>] [--extend <GRANT>]...` puts a parked run back to work, at its current node or at `--at <NODE>`, with a fresh timeout clock and a cleared check retry. `--extend` repeats:

| Grant | Effect |
| --- | --- |
| `visits=+N` | N is a positive integer. Adds N to every step's `max-visits` for this run. The grant survives resets. |
| `deadline=<WHEN>` | Replaces the run's deadline and arms it again. `WHEN` is an RFC 3339 instant or a local date or date-time; a bare date is local midnight. |

Any other grant spelling refuses with `invalid extension "<grant>"; use visits=+N or deadline=<when>`. An active run needs one of the two flags (`an active run needs --at <node> or --extend to resume`). `--at` moves an active run too: a live attempt at its current node ends `canceled` first, and while that attempt is still settling resume refuses (`the attempt at <node> is settling; retry resume after it finishes`). `--at` refuses an unknown node (`unknown sealed workflow node <node>`). A finished run refuses (`terminal workflow runs cannot resume`). The definition stays sealed: grants live on the run record.

The receipt says `resumed <address> at <node>`, then ` · <node> <used> of <max> visits` when that step is capped, `, every capped step +N` for an added visit grant, and ` · deadline <clock>` for an armed deadline. Extending an active run without `--at` says `extended <address>` instead, with ` · every capped step +N visits` and the same deadline part. Both end with a `workflow show` hint. When the armed deadline has already passed, stderr adds `warning: deadline <clock> has already passed; <destination> — <resume command>`, where the destination reads `the run goes to <target> at once`, and the command is the matching `--extend deadline=<when>`. A fired deadline stays disarmed on plain resume; use `--extend deadline=<WHEN>` to arm it again.

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
| `on.failed` | Where a failure goes when its node routes neither its cause nor `on.failed` ([failures](#failures)). |

| `[agent.<name>]` | Meaning |
| --- | --- |
| `agent` | Required. A kind, profile, or layout; a team only in a session with no `rest-check`. |
| `prompt-file` | Required. A file confined to the definition directory. |
| `then` | Required. One check, `@done`, or `@failed`: where the run goes when the attempt ends on its own. |
| `lifecycle` | Required. `turn` or `session`. |
| `rest-check` | A session only. A nonempty list of checks run at each clean rest. |
| `timeout` | The attempt's bound from its launch, on either lifecycle. |
| `max-visits` | At least 1. |
| `keep` | Either lifecycle. |
| `notes` | `false` omits the notes instruction. |
| `subscribe` | Signals forwarded to a session's leader; a `turn` step refuses it. |
| `on.asked` | Where a blocking question goes (either lifecycle). |
| `on.died`, `on.timeout`, `on.limited`, `on.visits`, `on.capacity`, `on.deadline`, `on.failed` | Failure routes. |
| `admission` | `window`, `occupancy` (`wait`, the default, `takeover`, or `share`), `max-parallel`, `stagger`, `throttle` (`off`), `give-up` (a duration from the first hold, or `forever`; unset, the run's give-up instant bounds the wait, and without one the step waits until the run is canceled; a `--skip-capacity` start never waits). |
| `account`, `mode`, `effort`, `fast`, `budget`, `system-prompt-file`, `worktree` | Launch settings, as the matching `rimz agents` flags. |

| `[command.<name>]` | Meaning |
| --- | --- |
| `run` or `shell` | Required, exactly one. |
| `timeout` | Required. |
| `then` | Required. One check. |
| `max-visits` | The visit bound. |
| `on.died`, `on.timeout`, `on.visits`, `on.deadline`, `on.failed` | Failure routes. |

| `[message.<name>]` | Meaning |
| --- | --- |
| `message` | Required. An agent address, `@<handle>` or `@<handle>#<channel>`. |
| `then` | Required. One node, `@done`, or `@failed`. |
| `prompt-file` | A file confined to the definition directory. |
| `session` | The agent session the address must resolve to. |
| `max-visits` | The visit bound. |
| `on.undelivered`, `on.visits`, `on.deadline`, `on.failed` | Failure routes. |

| `[test.<name>]` | Meaning |
| --- | --- |
| `run` or `shell` | Required, exactly one. Exit 0 answers `pass`, anything else `fail`. |
| `on.pass`, `on.fail` | Required. |
| `on.died`, `on.timeout`, `on.deadline`, `on.failed` | Failure routes. |
| `timeout` | Required when a route waits. |
| `wake` | Signals that re-run the test. |

| `[script.<name>]` | Meaning |
| --- | --- |
| `run` or `shell` | Required, exactly one. Stdout is the JSON answer. |
| `on.<outcome>` | One target per outcome the script answers, plus the failure routes `on.died`, `on.invalid`, `on.timeout`, `on.deadline`, and `on.failed`. |
| `timeout` | Required when a route waits. |
| `wake` | Signals that re-run the script. |

| `[judge.<name>]` | Meaning |
| --- | --- |
| `agent` | Required. One profile with a headless launch. |
| `prompt-file` | Required. A file confined to the definition directory. |
| `summary` | Required. |
| `on.<outcome>` | One target per outcome the judge answers, plus the failure routes `on.died`, `on.invalid`, `on.timeout`, `on.limited`, `on.deadline`, and `on.failed`. |
| `timeout` | Required when a route waits. |
| `wake` | Signals that re-run the judge. |
| `admission` | `window` only. |

| `[condition.<name>]` | Meaning |
| --- | --- |
| `cases` | Required, nonempty. Each case takes `when` and `outcome`, both required, and `for`. |
| `else` | Required. The outcome when no case is true. |
| `on.<outcome>` | Required for every case outcome and `else`. |
| `on.unknown` | Required. |
| `on.timeout`, `on.deadline`, `on.failed` | Failure routes. |
| `timeout` | Required when a route waits. |
| `provider`, `account` | The window terms' agent kind and login. |

| `[sleep.<name>]` | Meaning |
| --- | --- |
| `sleep` | Required. A nonzero duration. |
| `then` | Required. A node, `@done`, or `@failed`. |
| `when`, `for` | An early end: an ANDed term array, and how long it must hold. |
| `provider`, `account` | The window terms' agent kind and login. |
| `on.deadline`, `on.failed` | Failure routes. |

## Validate and schema

`rimz workflow validate [DIR] [--json]` checks one directory, or every directory under `~/.rimz/workflows/` whose name does not start with `.` (a crashed [`loop eject`](./loop.md#eject-a-task) leaves its staging copy under `.eject/`). A single normal path component names a workflow under that home, as in `run`; any other path is checked where it is, inside that home or not, and its name must still be a valid workflow name. A name may instead select a loop task's compiled graph, as in `show`, with the same `workflow:`/`task:` selectors. Profiles and teams resolve from the machine's definitions either way. The table has `WORKFLOW`, `NODE`, `KIND`, `RUNNER` (the run command, shell text, step layout, judge profile, `message`, `condition`, or `sleep`), `BACKGROUND` (whether the step's leader reports background work), and `EDGES`. An edge prints `outcome → target (source)`, or `outcome → target (source, mark)` when conditional or unable to fire; `source` is `explicit`, `on.failed` (the node's catch-all), `workflow.on.failed`, or `default`. A step's `asked` edge appears only when `on.asked` is set, always as explicit. A session's rest checks appear as explicit `rest-check → <check>` edges. Below the table, each error prints as `<path>: <message>`, then each warning as `warning: <path>: <reason>`, all on stdout. A definition that fails to parse reports one error, `<path>: <workflow>: <error>`. Any error exits 1 and adds `error: N workflow error(s)` on stderr; warnings alone exit 0. Condition and sleep stage names use the caller's effective teams plus `Done`; without a resolved workspace, a warning says stage names were not checked. Run admission always checks against the run's project.

Every table refuses a field it does not take, including a field another kind owns, at its dotted path with zero-based array indices, such as `agent.fix.takeover` or `condition.pr.cases.1.for`: `` agent.fix.takeover: <workflow>: agent.fix.takeover: unknown field `takeover`, expected one of `summary`, `agent`, … ``. A wrong-typed value refuses the same way, and so does a name two kinds share (`test.fix: <workflow>: test.fix: agent.fix has the same name; a node name is unique across every kind`). These are parse errors, so the definition reports the first one alone.

An invalid value in an optional field refuses at its path as a required field's does; nothing a definition writes is read as absent. That covers a blank or multiline `summary`, an unsupported `deadline`, a `timeout`, `stagger`, `for` or `sleep` that is not an `s`/`m`/`h`/`d` duration within `365d`, a zero `stagger`, `for` or `sleep`, an unsupported `window`, `max-visits` or `max-parallel` below 1, `give-up` that is zero, an invalid `wake` or `subscribe` entry (a bad selector or match, an agent lifecycle selector, a team selector other than exact `team.stage`, or a `prompt` on a check's `wake`), an unknown condition or sleep `provider`, a `provider` with no window term, an `account` without `provider`, an invalid agent `account`, `mode = "plan"`, a `budget` outside the `--budget` grammar, an invalid `worktree` name (empty is allowed), and a blank message `session`.

A check's route refuses when the check cannot answer it (`dead route: this check cannot answer that outcome`): a reserved word other than `failed` and the causes the check's kind raises, which a condition extends with `unknown`, or a condition outcome that no case or `else` declares. A route is a target string; a target that names no node, a failure route to `@wait` (`a failure route cannot wait; route it to a node, @done, or @failed`), and a work node's edge to `@wait` also refuse. Cross-field rules read the definition as written: a check-only cycle needs `@wait`, a cycle through work nodes needs `max-visits` on one of them, and every route except a `deadline` route counts as an edge. A node's `visits` or `capacity` route, resolved through the [chain](#failures), refuses when it can lead back to that node, at the path that names it; the walk does not follow a `deadline` route, which fires once per grant (`the visits route out of <node> leads back to <node>; it counts no visit, so route it where the run cannot return`).

Rows follow the graph from `start`, with a returning step immediately below the earlier check that sends it; unreachable nodes come last in name order. A check's edges follow case order without repeats, then `else`, then other explicit outcomes in name order; a step's start with `then`, then `asked`. One edge per cause the node's kind raises follows, in the order of the [cause table](#failures): `died`, `invalid`, `timeout`, `limited`, `undelivered`, `visits`, `capacity`, `deadline`, each with the target its chain resolves to. A session's `rest-check` edges come last, in list order. These row and edge orders also apply to JSON.

| Mark | Meaning |
| --- | --- |
| `never: no timeout` | The check has no timeout clock. |
| `never: no max-visits` | The step has neither visit bound. |
| `only with a deadline` | No deadline is sealed; a run needs a deadline grant. |

`--json` prints `{rows, warnings, errors}`, each error and warning carrying `path` and `message`. Each row has `runner = "run" | "shell" | "agent" | "message" | "condition" | "sleep"`, nullable `summary`, `run`, `shell`, `agent`, and `message` fields, `workflow`, `node`, `kind`, `do_background`, and resolved `edges`. Each edge keeps `outcome`, `target`, and `source`, and adds `fires`: `"yes"`, `"never"`, or `"with-deadline"`. Only `"never"` edges carry `because`: `"no-timeout"` or `"no-max-visits"`. An agent step's capacity edge always fires: a start wider than a machine cap raises `capacity` on any run, a `--skip-capacity` run included. Marks explain edges; they do not add validation warnings or relax admission rules.

A sleep's edges are explicit `then → <then>`, default `sleeping → @wait`, default `unknown → @wait` only with `when`, and `deadline`, in that order. There is no timeout edge. An evaluation error in `when` ends the run failed with no route. Definition rails and node details show `sleep`, `when`, `for`, and `then`, never the generated waiting edges or a generated case. A plain sleep journals `{outcome: "sleeping", recheck_at: "<entered_at + sleep>"}` at entry and `{outcome: "then", reason: "slept <sleep>"}` at expiry, inheriting the key into each object. Conditional sleeps use condition answers with `case: 0` for an early win; false is `sleeping`, unread is `unknown`, and an unfinished `for` rewrites a win to `hold`. These answers never replace the run's input or key. History hops show `sleeping → @wait` and `then → <then>`.

Each row's `kind` is the table word the node is written under (`agent`, `command`, `message`, `test`, `script`, `judge`, `condition`, or `sleep`), as is the `KIND` column. Command rows have `runner = "run" | "shell"`, the command in the corresponding field, `agent = null`, and `do_background = false`. Their edges are `then`, `died`, `timeout`, `visits`, and `deadline`, never `asked` or `capacity`; `BACKGROUND` prints `—`. Agent-step rows include a `capacity` edge, marked `yes`. Agent-only warnings do not apply to commands. Message-step rows have `runner = "message"`, the address in `message`, `agent = null`, and `do_background = false`; their edges are `then`, `undelivered`, `visits`, and `deadline`, and `BACKGROUND` prints `—`.

Turn steps and sessions with a `rest-check` warn when the leader adapter has no background reporting: a turn end may finish the attempt while provider background work remains. When auto-continue is off, agent steps warn at `agent.<step>` that a step paused on a provider limit waits for you, and judges warn at `judge.<check>` that a provider limit mid-attempt ends the attempt as `limited`. A Codex judge instead warns that a failed judge retries once, then fails `invalid`.

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

`show [RUN] --json` prints `{address, dir, record, journal}`: the short run address, work directory, run record, and history entries. Runs with kept notes add `notes`, the absolute folder path. When the sealed definition cannot load, it still prints the record and journal. An active attempt's optional `asking` is `{since, text, kind, handle}`, with `kind` one of `question`, `permission`, or `plan_approval`; `asked_ms` accumulates closed pauses. These fields pause a turn step's timeout, not the run deadline. Evaluation rows carry `origin = "check"` for a check's answer or `"engine"` for an engine-decided outcome. A forwarded signal journals `{event: "signal", node: <step>, signal: <name>, message: <message id>}`, with the subscribing step as its node even while a session is held at its rest check.

Optional `attempt.held_since` opens the shared pause at the earliest ask, limit, error, or budget-park evidence, clamped to the attempt's start and the observation time. It remains open through overlaps; `asked_ms` credits the interval once all such evidence clears.

Only blocking native asks pause an attempt. Async questions and explicitly raised questions remain actionable in `rimz asks` but do not create `asking` or pause the timeout.

Optional `record.restart` is `{resume: "session-id" | null, settled?: N}` while a manual replacement is pending. Null means a fresh start. `settled` names a previous attempt being stopped again whose end and cost are already journaled. The field disappears when the launch is published, when a route leaves the step for another node (a capacity give-up, a deadline) or `resume --at` moves the run to another node, or when the run parks or terminates.

`record.visits.<step>` is the step's visit count, since its last manual restart. Attempt journal rows and `record.prev_attempt` carry `routed_by` (the outcome that admitted the attempt) beside `key`, null when absent.

A capacity wait has `record.wait.outcome = "capacity"`, provider `kinds`, optional `hold` (the [typed cause](../../internals/harness/loops.md#the-start-throttle), absent in older records), and optional `gives_up_at` (absent for `forever`, and when neither the node nor the run bounds the wait), alongside the usual `node`, `detail`, `first`, `last`, and `count`. Closing the wait carries `hold` into its history entry. Giving up raises `capacity` through the [chain](#failures): its route's input is the failure input, `detail: {waited_ms, first}`, with any inherited `key`. A start wider than a machine cap raises `capacity` the same way before it counts a visit, `waited_ms` 0 unless a capacity wait was already open, with `never_fits: true` in its `detail`. Such a start is a configuration refusal: unrouted, it ends the run `failed`, and a loop task records a `failed` run with a strike, not `throttle skipped`.

Runs use `record.version = 9`, and RimZ refuses a record of any other version ([`cancel`](#cancel-and-resume) removes such a run). While checking an open session, optional `held` is `{attempt, entered_at, verdict?}`: the session attempt, the step's entry time, and the saved check verdict. A step's `attempt.visit` records its visit count at admission, including that launch or push-back; the context, `live_attempt`, `prev_attempt`, and attempt journal row carry that number. An attempt without a visit stamp uses the current count. Optional `attempt.push = {message, queued}` persists the push-back message id before delivery; `queued` means the host has confirmed enqueue, including an id already in live or retained history. Optional `attempt.judged` is `{agent_id, started_at, ended_at}`, the consumed leader turn end. A verdict is `{node, entered_at, at, target, object, reason}`, where `at` is the instant its evaluation was journaled. A closing attempt carries the verdict in optional `attempt.ending.route` until the stop settles.

Each turn-end decision or held-wait answer writes a journal row with `event = "turn_end"`, the session's `seq` and step `node`, `action = "kept" | "pushed" | "closed" | "discarded" | "interrupted"`, `turn = {agent_id, started_at, ended_at}`, and `check`. A saved verdict supplies optional `outcome` and `target`; `reason` explains an interruption or a moved-turn discard. A `pushed` row also carries optional `visit` and `message` and counts as a visit in history, the trail and the visit counts. Text history prints the action and any outcome, and the target only for `kept`, `pushed` and `closed`, since a discarded or interrupted verdict routed nowhere.

`work/context.json` adds `live_attempt` while a session with a `rest-check` is open with no staged ending: `{step, seq, visit, visits, key, started_at, handle, run_id, claim_path, head_start, turn_end}`. `turn_end` is the last consumed turn identity, or null. There is no outcome or end evidence in `live_attempt`; `prev_attempt` still describes the last ended attempt. During evaluation, the top-level context describes the check, without `visit` or `visits`; a push-back or discard rewrites it to the session's step view, a held `@wait` keeps the check view, and a close leaves it until the next publication.

Command-step attempts and `record.prev_attempt` carry nullable `code` (exit code, null after a signal or an unobserved exit) and `log` (the absolute `work/<seq>.step.log` path when the file exists). Agent attempts write both as null; records predating these fields still decode. Command steps have null `handle`, `run_id`, `children`, `limited`, `asked`, and `cost_usd`. A message-step attempt carries `handle` (the target, null when undelivered) and `message`, the id of the queued message, also on its journal `attempt` row; `code`, `log`, and `run_id` are null. Other attempts omit `message`. The hosting process samples `head_start` before the command and `head_end` after reaping it; `head_end` is null when another process settles it. `claim_path` still names `work/<seq>.claim.json`.

A command step records `ended` on exit 0 and follows `then`. It records `died` on another exit, signal, spawn failure, or log-open failure, and `timeout` when its step timeout interrupts it; each raises that [failure](#failures). A run deadline records `canceled` and raises `deadline`. Cancel records `canceled` and ends the run. A lost host records `died` with the reason `lost command-step host <pid>`, without rerunning the action. An interrupted command whose bound is no longer due records `canceled` and raises `died`. A child that exited on its own keeps its exit outcome even if a bound fell due before settlement. After `then`, the next check receives the original routed input; after a failure, the failure input. The command's evidence is in `prev_attempt`.

Text history keeps the attempt event line and adds `    exit <code|—> HEAD <start8|—> → <end8|—>` for a command step, followed by `    log: <path>` when present. An agent attempt names its handle instead of the exit. Any attempt with a reason adds `    reason: <text>`. A running command's headline has no agent handle.

A route into a step that has used its visits raises `visits` at that step with the [failure input](#the-failure-input): `detail.visits` is the step's count, `detail.max` its limit plus the run's visit grant. The journal's route input carries the same object. `visits=+N` raises the limits run-wide.

A test's answer is `{outcome, code, log}` plus an inherited `key` when present. Exit 0 is `pass`; another exit is `fail`. `code` is an integer. `log` is the absolute `work/<seq>.check.log` path, containing stdout and stderr together. Only `fail` has `reason` (`exited N`); output tails are not journaled. Both `on.pass` and `on.fail` are required. The other routes are the failure routes `on.died`, `on.timeout`, `on.deadline`, and `on.failed`; a spawn or log-open error, or a signal that kills the command, raises `died` (reason `killed by signal`), never `fail`. The check's optional timeout spans waits and evaluations, without inheriting the loop's default timeout. A timeout raises `timeout`, and a run deadline raises `deadline`. Both stop the command's process group rather than answering `fail`. A `fail` routed to `@wait` shows `said fail` in the waiting line.

A script keeps stdout as its JSON answer and writes complete stderr, including an empty stream, to `work/<seq>.check.log` once it settles. Invalid answers still journal the stderr tail. A rest check's push-back names its answering check, then its reason, then `output: <path> (<size label>)`, or `no output` for an empty or missing file, before the closing instruction. Tests and scripts use `.check.log`; a judge uses its whole saved `.check.json` answer. No tail or file content is copied into the prompt. Condition push-backs keep their term explanations.

A condition's answer has `outcome`, `readings` (a map from term key to string or null), and the winning zero-based `case` (omitted for `else` and `unknown`). The inherited run key is included when present; cases do not set keys. `unknown` includes a `reason` naming each unread key. While a case's `for` hold runs, the outcome is `hold`; `record.wait` carries `case`, `detail` (the case's outcome), `first`, `last`, `count`, and `recheck_at`. Without a hold, `wait.case` is omitted. An `unknown` answer routed to `@wait` keeps a running hold, its `first` included, while the held case reads true or unknown, and sets `wait.unknown` to that answer's reason (empty when it gives none) until a true answer of the held case clears it; the hold closes when that case reads false, another case answers, or a route leaves the check. A condition routes `unknown` only through its required `on.unknown`, and a conditional sleep's `unknown` waits. A condition's `timeout` and `deadline` take the [chain](#failures); an evaluation error, as opposed to an unread term, ends the run failed with no route. The eight causes, `failed`, `asked`, `unknown`, and `hold` are reserved outcome words; `on.hold` is a dead route, so it refuses.

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
