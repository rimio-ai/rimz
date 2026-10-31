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
rimz workflow resume 'babysit-pr#feat-x' --at pr --extend visits=+1
rimz workflow cancel 'babysit-pr#feat-x'
```

The [global flags](../cli.md#global-flags) apply. Every subcommand except `run` works from any directory: a run id resolves across every room on the machine.

`show`, `resume`, and `cancel` accept a run address `<workflow>#<lane>`, a workflow name, or a full run id (`wf_` and 32 hexadecimal digits). The lane is the basename of the recorded scope path, even after that checkout is removed. Root-checkout and project-scoped runs use the project directory's name. A requested `feat/x` also matches `feat-x`. When a target begins with `-`, put options before `--` and the target after it.

An address selects its live run; `show` falls back to the newest finished run when none is live. Two live runs sharing an address refuse and list their ids and checkouts. A workflow name in `show` draws its definition, then lists that workflow's live runs, never one run's detail; a known workflow with none ends with `no active or parked runs of <name>`. For `resume` and `cancel`, a workflow name prefers the caller's checkout or its project's project-scoped run, then the only live run, otherwise lists commands to choose one. Matching runs in the caller's room take precedence; without a local match, several rooms refuse with `--root` commands. Each command a refusal lists names its run by address, or by id where another candidate in the same room shares that address, and carries `--root` when the run is outside the current directory's room. The global `--root` chooses the caller's room. Run ids remain unambiguous across rooms.

## Run a workflow

`rimz workflow run <NAME|DIR> [-w <WORKTREE>] [--prompt <TEXT>]`

| Argument or flag | Meaning |
| --- | --- |
| `<NAME\|DIR>` | A single path component is a name under `~/.rimz/workflows/`; anything else is a directory, which must lie inside `~/.rimz/workflows/`. `.` is the current directory. |
| `-w, --worktree <WORKTREE>` | Select an existing RimZ-owned worktree instead of the current checkout. It never creates one. Project scope overrides this selection. |
| `--prompt <TEXT>` | Stored unchanged, appended to every agent step's prompt, and exposed as `prompt` in `context.json`. |

Before creating a run, the command validates the definition and refuses on any error. It needs an existing room (`rimz start` creates one). With `scope = "worktree"`, nodes run in the selected checkout; with `scope = "project"`, they run at the project root, even when invoked from an owned worktree. One active or parked run holds each `(workflow, scope path)` pair; a second start refuses and names the existing run. The CLI validates before it asks for that slot, so a broken definition still refuses on validation first. On success it prints the run id, then the run's work directory (`RIMZ_WORKFLOW_DIR`), then a `workflow show` hint for that run. The first two lines keep their scripting format. The first advance starts in the background. The work directory includes `notes/` for node-owned cross-node scratch and `signs/` for step guidance. Every evaluation and attempt takes a new `seq`, a retry included, so name a scratch write after the item it records to make a repeat overwrite it.

When `-w` selects a project-scoped workflow, stderr prints `note: <workflow> has scope = project; the run is at <project root>, not <worktree path>`. The command still exits 0 and stdout is unchanged.

A step selects exactly one of `agent`, `run`, or `shell`, and requires `then` naming a check. A run or shell step requires `timeout` and accepts the common visit bounds and `on.cap`. `run` is a literally passed argv, with slash-containing words confined to the definition directory; `shell` runs through `sh -c` without path confinement. Command steps refuse `prompt-file`, `lifecycle`, `window`, `takeover`, `throttle`, `capacity-wait`, `on.capacity`, and `on.asked`, and never wait for checkout occupancy or capacity. Their stdin is closed, both streams go to `work/<seq>.step.log`, and they inherit the workspace pin and workflow environment.

An agent step requires `prompt-file`. Its `agent` accepts the fresh-launch layout grammar, including agent and command cells. `lifecycle` is `turn` by default, `stay` to keep the attempt live until the leader's session exits, or `until` to check `then` at each eligible leader turn end and close when its route is neither this step nor `@wait`. A staying step (`stay` or `until`) has neither `timeout` nor `on.asked`. A kept `@wait` on `until` has no timed recheck: the next leader turn end checks again. Teams, command-only layouts, and ambiguous prompt leaders refuse with a field-specific fix. Judge checks still take one profile. An agent step's `window` gates every agent cell's provider.

Agent steps of automatically loop-started runs also ask the [start throttle](./loop.md#the-start-throttle) before taking over a checkout. `throttle = "off"` bypasses it. `capacity-wait` accepts a positive `s`, `m`, `h`, or `d` duration, or `forever` (the default). A held step spends no visit and retries about once a minute. Giving up follows `on.capacity`, default `@failed`; it may instead name `@done` or a node, but not `@user` or `@wait`, since capacity give-up never parks a run. A window or occupancy wait restarts the capacity clock. The run deadline still applies. Runs created by `workflow run` or `loop fire`, and runs detached by `loop remove`, bypass capacity, as do checks, command steps, and already-open staying sessions.

## Inspect definitions

`rimz workflow show <WORKFLOW>` draws the current definition in walk order, with returning steps hung beneath their earlier sending check. `→` points to a later node and `↑` to an earlier node or itself. Unreached nodes follow under `unreachable from <start>`. The header names the summary, scope, loop tasks that start this workflow in the caller's project (machine tasks only outside a room), and any configured deadline route. Without a loop task it gives the by-hand command.

Each node shows its runner, summary when set, routes, and routing limits. Condition cases retain their order and hold durations, followed by `none true`; the note names where an unread term goes. Timeouts, visit caps, windows, takeover, bounded capacity waits, and staying lifecycles stay beside their nodes. Edges that cannot fire are omitted; uniform turn-end, invalid-check and unset-deadline defaults are stated once in the footer, only when used. `validate` shows the omitted edges with their marks. The footer names the source file and a command for one node in full. The live-runs table follows the definition.

A missing or unloadable definition prints `definition unavailable` with a `workflow validate` command, then still lists its runs and exits 0. An unknown workflow with neither a definition directory nor runs refuses. `show <WORKFLOW> --json` keeps its live-runs array and does not include the rail.

## Inspect runs

`rimz workflow list [--json]` prints every active and parked run across rooms, without column headers. NEEDS YOU holds parked runs and active runs with a blocking ask, longest waiting first. ACTIVE holds the rest, sorted by address. Rows are `<glyph> <address>  <node>  <detail>  <age>`: `?` for parked or asking, `☾` for an active wait, `⢿` for other active work, from the configured glyph set. Age starts at the park, at the opening of an ask, or at node entry otherwise. Detail truncates before age at narrow terminal widths. NEEDS YOU ends with a `workflow show` hint for its first run. Several rooms get headings, the caller's room first, and hints for other rooms carry `--root`. Finished runs are omitted.

Detail is `asking you: <handle> (<kind>): <one-line text>` for a blocking ask; otherwise a live staying session reads `with you`, with `; checking <check>` while held and `; <wait phrase>` when waiting. Other runs print the [park phrase](#park-reasons), [wait phrase](#the-waiting-line), `running` (with ` · visit <n> of <max>` when capped), or `checking`. JSON `detail` uses the same phrase, without node or age. A single-run `show` says `<step> is asking you` while asking, otherwise `<step> is with you` for a live staying attempt. Its session attempt is `held.attempt` while checking, otherwise `attempt`; its `lifecycle` is `stay` or `until`, omitted for turn attempts. An open blocking ask is stored in `asking`; `asked_ms` is the total closed pause time in milliseconds. Both are omitted when empty or zero. While held, NODE names the check; `show` keeps the session's step and handle in its headline and names the current check below it.

`rimz workflow show <RUN> [--history | --json]` opens with the address and current state. A wait names its node, elapsed time, recheck and the next bound's time and target; a hold names its outcome, duration and the next bound's time and target. The forecast picks the earliest unfired deadline, timeout or hold completion, in that order on ties. A running step shows its visit limit and agent handle when available. Asking shows the question and says to answer in the pane or with `rimz answer`; a turn step's timeout pauses while asking. Finished runs say `done`, `failed`, or `canceled`, aged from termination. The next line names when and how the run started (by hand or by loop task), the number of completed attempts, and their known total cost, including judge evaluations. It omits cost when none is recorded, and never estimates a live attempt's spend.

The current-node block shows the latest condition readings (`unread` for null), wait count and recheck, hold progress, running step, or full ask text. A takeover wait adds `takeover:` lines naming the agents that hold the scope path (`blockers unavailable` when the agent roster or the project's worktrees cannot be read). A parked run instead explains [why it stopped](#park-reasons) and prints `you can` commands. These commands, history footers, receipt hints and NEEDS YOU hints use the address when it selects this run, otherwise the run id, and add `--root` for another room. A visit cap lists the capped step's attempts and the refused visit; a per-key cap lists only that key's attempts. A timeout shows the flushed wait and the last shell-check log, and `invalid` shows the recorded stderr tail.

`visits  <step> <used> of <max>` lists each visited capped step, with the run's grant included in the limit. A per-key cap adds `<step> <used> of <max> for key <json key>` in recorded key order (`null` for unkeyed visits). Uncapped steps print no visit rows. The `trail` projects routing decisions from the sealed specification and journal, folding consecutive matching check answers as `×N`. It includes resumes, parks and termination, keeps at most the newest eight hops with `+N earlier` for the rest, and ends in `now` while live. The footer names the home-relative work directory and the command for full history.

`--history` replaces the compact view with the same headline and detailed journal history in recorded order, using local clocks and readable facts rather than raw JSON. Evaluations show outcomes and, only when they routed, targets, along with readings, reasons, shell logs, judge cost, and invalid stderr tails. Attempts show visits, handles or command exits, duration, cost, short HEAD before and after, logs, reasons, stopped and live children, provider reset, questions, and digests. Routes show targets and input fields; waits show outcome, count and span; turn ends show action, check, outcome and target. Parks, resumes, cancellation, terminal states and wake signals retain their recorded facts. `--history` requires an address or id, not a workflow name, and conflicts with `--json` (usage error, exit 2).

Adjacent evaluations with the same node, origin, outcome, and condition case index fold into one row, regardless of changes to their reason or other answer fields. Adjacent wakes fold when their node and signal agree; a wake between evaluations keeps its own row and breaks their run. A folded row uses the newest time, `#first–last` (or `#—` without sequences), `×N over <span>`, the newest answer, and the judge cost its members recorded, summed. The span runs from the oldest to the newest member. A closing check-wait summary takes no row when any earlier evaluation row at its node with its outcome overlaps the wait's inclusive span. Other waits, including step waits and waits whose evaluations are missing, print `×N over <span>` when N > 1, using the wait's first and last times, followed by ` · <detail>` when present. Omitting a summary does not break an evaluation run. `--json` keeps every journal entry, including closing waits, without folding.

### The waiting line

A run at a wait carries a shared phrase without node or `since`; a single-run `show` puts it in its state headline and current-node block. Recheck, deadline and history clocks use local `HH:MM` today, `Mon HH:MM` within six days in either direction, and `YYYY-MM-DD HH:MM` beyond that. Capacity waits retain the shared sidebar clock format.

```console
$ rimz workflow show ci-watch
ACTIVE
  ☾ ci-watch#feat-x  review  said pending, ×1, recheck 16:09  0s
```

| Part | Meaning |
| --- | --- |
| `said <outcome>` | A check answered an outcome routed to `@wait`. |
| `holding <outcome> <elapsed>/<for>` | A condition case keeps winning while its hold duration runs. |
| `unknown: <keys and reasons>` | A condition cannot read enough data to choose its first matching case. Bare `ci` can name a local HEAD not on the PR or CI read at a different PR head; see [JSON shapes](#json-shapes) for the reason forms. |
| `window <term>` | An agent step or judge whose provider `window` is closed, with the failing term and its reading. |
| `takeover` | The checkout holds an agent mid-turn, so the step cannot evict it yet. |
| `occupied` | The checkout holds an idle agent and the step has no `takeover = true`. |
| `waiting for capacity (<kinds>)` | A loop-started agent step is held by the start throttle. Its give-up time is absent for `forever`; reason and count follow ` · `. |
| `×N` | How many advances found it still waiting. `show --history` omits this count from the waiting phrase only when exactly one history row states the wait with the same count. `workflow list` and the loop commands always retain it. |
| `recheck` | The check's `recheck_at`, before which nothing evaluates; a signal matching that check's subscription clears it. For a `for` hold it is when the hold completes, and subscribed signals and the minute tick re-sample the case before then. |

## Cancel and resume

`rimz workflow cancel <RUN>` ends a run as `canceled` and stops its live script, shell, judge, or step attempt. It prints `canceled <address>`. An already-finished run id prints `<address> is already <state>; unchanged` and still exits 0. Both receipts end with a `workflow show` hint.

`rimz workflow resume <RUN> [--at <NODE>] [--extend <GRANT>]...` puts a parked run back to work, at its current node or at `--at <NODE>`, with a fresh timeout clock and a cleared check retry. `--extend` repeats:

| Grant | Effect |
| --- | --- |
| `visits=+N` | Adds N to every step's `max-visits` and `max-visits-per-key` for this run, for every key. |
| `deadline=<WHEN>` | Replaces the run's deadline and arms it again. `WHEN` is an RFC 3339 instant or a local date or date-time; a bare date is local midnight. |

An active run accepts `--extend` alone; `--at` needs a parked run. A finished run refuses. The definition stays sealed: grants live on the run record.

The receipt says `resumed <address> at <node>`, with the current step's used and granted visit limit when capped, any added visit grant, and its armed deadline. Extending an active run says `extended <address>` instead. Both end with a `workflow show` hint. Stderr warns when a parked run re-enters a step with an exhausted visit bound; an active run's admitted attempt does not trigger that warning. Either receipt warns about an armed deadline already past. Each warning names the immediate destination and suggests the matching `--extend`. A fired deadline stays disarmed on plain resume; use `--extend deadline=<WHEN>` to arm it again.

```console
$ rimz workflow resume 'repair#feat-x' --extend 'visits=+1'
resumed repair#feat-x at work · work 2 of 3 visits, every capped step +1
→ rimz workflow show 'repair#feat-x'
```

## Validate and schema

`rimz workflow validate [DIR] [--json]` checks one directory, or every directory under `~/.rimz/workflows/`. A single normal path component names a workflow under that home, as in `run`. The table has `WORKFLOW`, `NODE`, `KIND`, `RUNNER` (the run command, shell text, step layout, judge profile, or `condition`), `BACKGROUND` (whether the step's leader reports background work), and `EDGES`. An edge prints `outcome → target (source)`, or `outcome → target (source, mark)` when conditional or unable to fire; `source` is `explicit` or `default`. A step's `asked` edge appears only when `on.asked` is set, always as explicit; staying steps have none. Errors and `warning:` lines follow. Any error exits 1. Condition stage names use the caller's effective teams plus `Done`; without a resolved workspace, a warning says stage names were not checked. Run admission always checks against the run's project.

Rows follow the graph from `start`, with a returning step immediately below the earlier check that sends it. Nodes reachable only through `workflow.on.deadline` follow the start component; unreachable nodes come last in name order. Edges follow case order without repeats, then `else`, then other explicit outcomes in name order, then engine triggers: `unknown`, `timeout`, `invalid`, `deadline` for checks; `then`, `asked`, `cap`, `capacity`, `deadline` for steps. These row and edge orders also apply to JSON.

| Mark | Meaning |
| --- | --- |
| `never: no timeout` | The check has no timeout clock. |
| `never: no max-visits` | The step has neither visit bound. |
| `never: capacity-wait is forever` | Capacity admission never gives up. |
| `never: throttle is off` | The step bypasses capacity admission. |
| `never: a judge retries once, then parks at @user` | A judge never follows its `invalid` edge, even an explicit one. |
| `only with a deadline` | No deadline is sealed; a run needs a deadline grant. |
| `only when a loop task starts the run` | Capacity can give up in an automatic loop-started run, not a by-hand run or `loop fire`. |

`--json` prints `{rows, warnings, errors}`, each error and warning carrying `path` and `message`. Each row has `runner = "run" | "shell" | "agent" | "condition"`, nullable `summary`, `run`, `shell`, and `agent` fields, `workflow`, `node`, `kind`, `do_background`, and resolved `edges`. Each edge keeps `outcome`, `target`, and `source`, and adds `fires`: `"yes"`, `"never"`, `"with-deadline"`, or `"in-loop-runs"`. Only `"never"` edges carry `because`: `"no-timeout"`, `"no-max-visits"`, `"capacity-wait-forever"`, `"throttle-off"`, or `"judge-retries"`. Marks explain edges; they do not add validation warnings or relax admission rules.

Command-step validation rows have `kind = "step"`, `runner = "run" | "shell"`, the command in the corresponding field, `agent = null`, and `do_background = false`. Their edges are `then`, `cap`, and `deadline`, never `asked` or `capacity`; `BACKGROUND` prints `—`. Agent-step rows include a `capacity` edge to `@failed` by default, marked according to `throttle` and `capacity-wait`. Agent-only warnings do not apply to commands.

Turn and `until` steps warn when the leader adapter has no background reporting: a turn end may finish the attempt while provider background work remains. Staying steps skip the auto-continue warning; live provider limits do not end their sessions.

`rimz workflow schema` prints the JSON Schema of `workflow.toml`. The `[workflow]` header accepts an optional `summary`: one nonempty line describing the graph. Blank or multiline values refuse at `workflow.summary`; omission never warns. Checks and steps accept their own summaries under the same rule.

## Loop tasks

A [loop task](./loop.md) starts a workflow with `workflow = "<name>"` in place of `agent` or `wait`:

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
| `budget`, `budget-per-day` | Ignored: a workflow fire is not gated on spend, and step costs are not counted against the task. The turn, room, and account caps still park a step agent like any agent in the room. |

`loop show` joins each checkout to its recorded run, finished runs included. `loop list` keeps only active and parked runs; its launched-worktree count includes finished entries.

`stay`, `takeover`, `subscribe`, `check`, `judge`, `throttle`, and `capacity-wait` are refused, each with the workflow node that owns the concept. A `when` task launches its workflow once per checkout: subsequent scheduled fires start nothing and record nothing, whether that run is active, parked, or finished. Project-scoped launches cover all of the task's checkouts, including later ones. `rimz loop fire <task>` bypasses this memory and replaces the entry only when a new run starts. `rimz workflow run` starts an independent run without changing the task's entry. Task replacement preserves the entries; rename moves them and removal clears them. Removal also detaches the task's active and parked workflow runs, which continue as independent runs.

An unrecorded checkout retries a sustained-true condition every five minutes. A fire that finds an active or parked run records `workflow skipped` with its id and adds no strike, checking before it reads the definition or prompt. If that run belongs to this task, the fire repairs its launch entry, so later scheduled fires are silent. A run started independently or by another task is not adopted, and the task retries until that run ends. Schedule, signal, and watch workflow tasks keep admitting a new run on each fire unless a live run holds the scope; finished runs do not block them.

## Park reasons

A parked run appears under NEEDS YOU in `workflow list` and, when it belongs to a loop task, `loop list`. The sidebar cockpit also surfaces it; `show` explains why and prints the available next commands.

The list's park phrase is `<reason>[: <text>]`. Cap, timeout, and deadline phrases derive `cap: <used> of <max> visits`, `timeout: waited <timeout>`, and `deadline: passed <clock>` from the sealed spec and record; other reasons keep the recorded text on one line.

| Reason | Cause |
| --- | --- |
| `user` | An edge routed to `@user`; the text is the check's `reason`. |
| `cap` | A step reached `max-visits` or `max-visits-per-key`, and `on.cap` defaults or routes to `@user`. |
| `deadline` | The run's deadline passed, and `on.deadline` defaults or routes to `@user`. |
| `timeout` | A check's `timeout` passed, and `on.timeout` defaults or routes to `@user`. |
| `invalid` | A script or shell check could not evaluate (including invalid script output), and `on.invalid` defaults or routes to `@user`. |
| `asked` | A turn step's leader or child blocked on a question or permission prompt and `on.asked` routes to `@user`; the text is the prompt. |
| `check` | A check evaluation failed twice. |

Script process failures say `<script> exited N` or `<script> killed by SIGTERM` (using the signal's name when known). `<script>` is the first `run` word containing `/`, or its first word when none does. Judge process failures say `judge exited N` or `judge killed by SIGKILL`; provider errors or invalid provider output take precedence over the exit status. Existing runs retain their recorded text.

Routes, step launches, parks, and ends append `workflow_*` assists ([stats](./stats.md#the-assist-timeline)). A merged wait appends one route assist when it closes, with its final `count`; an open wait has no route assist yet. Stats renders a closed wait as `workflow <workflow> <node> waited <outcome> ×<count>`.

## JSON shapes

`list --json` and `show <workflow> --json` print arrays of `{id, address, workflow, scope, state, node, since, detail}`. `scope` is the scope path: the checkout, or the project root for `scope = "project"`. `since` remains node entry, not the human row's state-specific age. `detail` is the human phrase above. No existing key is removed, but consumers matching the old `waiting: <node>,` prefix must use the separate `node` and `since` fields instead.

`show --json` prints `{address, dir, record, journal}`: the short run address, work directory, run record, and history entries. An active attempt's optional `asking` is `{since, text, kind, handle}`, with `kind` one of `question`, `permission`, or `plan_approval`; `asked_ms` accumulates closed pauses. These fields pause a turn step's timeout, not the run deadline. Evaluation rows carry `origin = "check"` for a check's answer or `"engine"` for an engine-decided outcome.

A capacity wait has `record.wait.outcome = "capacity"`, provider `kinds`, and optional `gives_up_at` (absent for `forever`), alongside the usual `node`, `detail`, `first`, `last`, and `count`. Giving up journals a `capacity` route with input `{outcome: "capacity", step, reason, kinds, waited_ms}` and any inherited `key`.

Runs whose sealed spec contains `until` use `record.version = 2`; other runs remain version 1. While checking an open session, optional `held` is `{attempt, entered_at, verdict?}`: the session attempt, the step's entry time, and the saved check verdict. Optional `attempt.judged` is `{agent_id, started_at, ended_at}`, the consumed leader turn end. A verdict is `{node, entered_at, at?, target, object, reason}`; every new verdict writes `at`, the instant its evaluation was journaled. Older verdicts have no `at`, so a wait merged from one uses the merge instant. A closing attempt carries the verdict in optional `attempt.ending.route` until the stop settles. Older binaries refuse version 2 and skip those runs in lists.

Each consumed turn end writes a journal row with `event = "turn_end"`, the session's `seq` and step `node`, `action = "kept" | "closed" | "discarded" | "interrupted"`, `turn = {agent_id, started_at, ended_at}`, and `check`. A saved verdict supplies optional `outcome` and `target`; `reason` explains an interruption or a moved-turn discard. Text history prints the action and any outcome and target.

`work/context.json` adds `live_attempt` while an `until` session is open with no staged ending: `{step, seq, visit, visits, key, started_at, handle, run_id, claim_path, head_start, turn_end}`. `turn_end` is the last consumed turn identity, or null. There is no outcome or end evidence in `live_attempt`; `prev_attempt` still describes the last ended attempt. During evaluation, the top-level context describes the check, without `visit` or `visits`; a keep or discard rewrites it to the session's step view, while a close leaves it until the next publication.

Command-step attempts and `record.prev_attempt` carry nullable `code` (exit code, null after a signal or an unobserved exit) and `log` (the absolute `work/<seq>.step.log` path when the file exists). Agent attempts write both as null; records predating these fields still decode. Command steps have null `handle`, `run_id`, `children`, `limited`, `asked`, and `cost_usd`. The hosting process samples `head_start` before the command and `head_end` after reaping it; `head_end` is null when another process settles it. `claim_path` still names `work/<seq>.claim.json`.

A command step records `ended` on exit 0, `died` on another exit, signal, spawn failure, or log-open failure, and `timeout` when its step timeout interrupts it; these follow `then`. A run deadline records `canceled` and follows `on.deadline`. Cancel records `canceled` and ends the run. A lost host records `canceled` with a reason and follows `then`, without rerunning the action. An interrupted command whose bound is no longer due follows `then` as `canceled`. A child that exited on its own keeps its exit outcome even if a bound fell due before settlement. The next check receives the original routed input; the command's evidence is in `prev_attempt`.

Text history keeps the attempt event line and adds `    exit <code|—> HEAD <start8|—> → <end8|—>` for a command step, followed by `    log: <path>` when present. An agent attempt names its handle instead of the exit. Any attempt with a reason adds `    reason: <text>`. A running command's headline has no agent handle.

A visit cap routes `on.cap` with `{outcome: "cap", step, bound: "max-visits" | "max-visits-per-key", visits, max}`, plus the inherited `key` when present. The total cap is checked first and wins when both trip. `visits` is the tripped bound's count; `max` is its limit plus the run's visit grant. The journal's route input carries the same object.

A shell answer is `{outcome, code, log}` plus an inherited `key` when present. Exit 0 is `pass`; another exit or signal is `fail`. `code` is an integer, or null after a signal. `log` is the absolute `work/<seq>.check.log` path, containing stdout and stderr together. Only `fail` has `reason` (`exited N` or `killed by signal`); output tails are not journaled. Both `on.pass` and `on.fail` are required. The only additional routes are `on.timeout` and `on.invalid`, each defaulting to `@user`; command spawn or log-open errors are `invalid`. The check's optional timeout spans waits and evaluations, without inheriting the loop's default timeout. A timeout routes `timeout`; a run deadline follows `workflow.on.deadline`. Both stop the shell's process group rather than answering `fail`. A `fail` routed to `@wait` shows `said fail` in the waiting line.

A condition's answer has `outcome`, `readings` (a map from term key to string or null), and the winning zero-based `case` (omitted for `else` and `unknown`). A case's fixed string or integer `key`, or the inherited run key, is included when present. `unknown` includes a `reason` naming each unread key. While a case's `for` hold runs, the outcome is `hold`; `record.wait` carries `case`, `detail` (the case's outcome), `first`, `last`, `count`, and `recheck_at`. Without a hold, `wait.case` is omitted. `unknown` defaults to `@wait`; `timeout` and `invalid` default to `@user`. Explicit `on` entries override those defaults. `hold`, `unknown`, `timeout`, `invalid`, `cap`, `capacity`, and `deadline` are reserved outcome words, and `on.hold` is refused.

The bare-`ci` commit fence sets `readings.ci` to null. When that makes the answer `unknown`, `reason` includes one of these, using the first seven characters of each commit OID (or all of a shorter OID):

- `ci (local HEAD <l7> is not on the PR, PR head <p7>)`
- `ci (read at <c7>, PR head <p7>)`

The local-HEAD reason takes precedence over the CI-commit reason; the pre-entry-probe reason takes precedence over both. The same text follows `unknown: ` in the waiting line and JSON `detail`, and remains in the journaled evaluation answer printed by `show` history.

`rimz loop list --json` carries `workflow_runs` for a workflow task: one object per active or parked run the task started in that room, `{id, address, checkout, state, node, since}` plus `reason` and `text` on a parked run. Both list and show add the shared `detail` when nonempty. `since` stays node entry; a human parked summary's age starts at the park.

`rimz loop show --json` also includes every run named by the task's condition launch memory, each once, including terminal runs whose task was renamed. A run canceled while parked keeps its recorded `reason` and `text` in JSON, without appearing as parked in human output. A run whose record is gone has only `id`, `checkout` (the launch entry's key), `state: "gone"`, and `since` (the launch time). Fan-out `worktrees[].run` repeats the matching `workflow_runs` object; project-scoped launches share one run across checkout rows. The human WORKTREES table names those runs, and RUNS lists only runs no worktree row names, folded rows included.

## Exit codes

`0` on success. `1` when a command refuses (an unknown run id, a duplicate start, a terminal resume, a validation error) or fails. `2` on a usage error.
