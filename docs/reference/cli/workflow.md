# Workflow CLI

`rimz workflow` starts, inspects, and steers durable workflow runs: a graph of checks and steps under `~/.rimz/workflows/<name>/`, driven by the run record rather than by any agent's memory. The [workflows guide](../../guide/workflows.md) teaches the format with worked examples; how the engine decides is [workflow internals](../../internals/harness/workflow.md).

```sh
rimz workflow schema                              # the spec's JSON Schema
rimz workflow validate                            # every machine workflow
rimz workflow validate ~/.rimz/workflows/babysit-pr --json
rimz workflow run babysit-pr                      # in the current checkout
rimz workflow run babysit-pr -w feat-x --prompt 'merge before Friday'
rimz workflow run babysit-pr --at pr --input '{"outcome":"green"}' --once # try one node
rimz workflow list                                # active and parked runs, every room
rimz workflow list --all -n 50                    # then the runs that ended, newest first
rimz workflow show 'babysit-pr#feat-x'
rimz workflow show 'babysit-pr#feat-x' --history  # detailed history, folded repeats
rimz workflow show babysit-pr                     # definition rail, then live runs
rimz workflow show babysit-pr pr                  # one definition node in full
rimz workflow extend 'babysit-pr#feat-x' pr --visits +1
rimz workflow resume 'babysit-pr#feat-x' --at pr
rimz workflow restart 'babysit-pr#feat-x' --continue
rimz workflow cancel 'babysit-pr#feat-x'
rimz workflow submit pass --reason 'tests cover it' # from a judge's own pane
```

The [global flags](../cli.md#global-flags) apply. Every subcommand except `run` works from any directory: a run id resolves across every room on the machine.

A run is active (a node is working or waiting), parked (stopped at a node until a command moves it), or finished as `done`, `failed`, or `canceled`. A try (`run --once`) that ended done reads `tried` in its views. "Unfinished" means active or parked.

## Run addresses

`show`, `resume`, `extend`, `restart`, and `cancel` take one of three targets:

| Target | Example | Selects |
| --- | --- | --- |
| Run address `<workflow>#<checkout>` | `babysit-pr#feat-x` | That workflow's unfinished run in that checkout. `show` falls back to the newest finished run that this RimZ's record version wrote when none is unfinished. Any finished run, whatever version wrote it, is reached by the id `rimz workflow list --all` prints. |
| Workflow name | `babysit-pr` | In `show`, the definition and its live runs. In `resume`, `extend`, `restart`, and `cancel`, the caller's run (below). |
| Run id | `wf_` and 32 hexadecimal digits | That run, finished or not, in any room. |

The checkout name is the basename of the run's recorded scope path, kept after that checkout is removed. Root-checkout and project-scoped runs use `main` (`review#main`). The project directory's name and the project path do not name them. A requested `feat/x` also matches `feat-x`.

- A target with `#` but an empty workflow or checkout name, or with a second `#`, is a usage error (exit 2). When a target begins with `-`, put options before `--` and the target after it.
- `resume`, `extend`, `restart`, and `cancel` reach a finished run only by its id.
- Two unfinished runs sharing an address refuse and list each id and checkout with a command for it. A real run and a [try](#trying-one-node) sharing one do not: the address selects the real run.
- For `resume`, `extend`, `restart`, and `cancel`, a workflow name selects the run in the caller's checkout, or its project's project-scoped run, then the only unfinished run. Otherwise it lists commands to choose one.
- Matching runs in the caller's room take precedence. Without a local match, matches in several rooms refuse with `--root` commands. Each listed command names its run by address, or by id where another candidate in the same room shares that address or the run is a try, and carries `--root` when the run is outside the current directory's room. The global `--root` chooses the caller's room.

## Run a workflow

`rimz workflow run <NAME|DIR> [-w <WORKTREE>] [--prompt <TEXT>] [--deadline <WHEN>] [--give-up <WHEN>] [--skip-capacity] [--at <NODE>] [--input <OBJECT>] [--once] [--detach]`

| Argument or flag | Meaning |
| --- | --- |
| `<NAME\|DIR>` | A single path component is a name under `~/.rimz/workflows/`. Anything else is a directory, which must exist inside `~/.rimz/workflows/` (otherwise `<NAME\|DIR>: directory must exist inside <home>`). `.` is the current directory. |
| `-w, --worktree <WORKTREE>` | Run in this RimZ-owned worktree instead of the current checkout, created from the configured base when missing, as an agent launch's `-w` creates one. A linked worktree of that name that RimZ does not manage is entered only on confirmation at a terminal, and refused without one; a directory there that is not a linked worktree of the project refuses. A workflow with `scope = "project"` refuses it before anything is created. |
| `--prompt <TEXT>` | Stored unchanged, appended to every agent step's prompt and every message step's message, and exposed as `prompt` in `context.json`. The value may begin with `-`. For a value spelled like a flag (`--help`), write `--prompt=<TEXT>`. |
| `--deadline <WHEN>` | The run's deadline, in place of the definition's `[workflow].deadline`; with neither, the run has no deadline. `<WHEN>` is a [workflow time](#workflow-times), resolved when the run is created and stored on the run, where `extend --deadline` replaces it. Any other value exits 2 with the forms. An instant already past fires the deadline at the run's first advance. |
| `--give-up <WHEN>` | The run's give-up instant. A capacity wait at an agent step without its own `admission.give-up` gives up at `<WHEN>` and fails with cause `capacity`. `<WHEN>` is a [workflow time](#workflow-times), resolved when the run is created. Any other value exits 2 with the forms. An instant already past gives up at the first hold. |
| `--skip-capacity` | The run's agent steps launch past every capacity hold, workflow and step limits and the machine [start throttle](../../guide/configuration.md#agent-start-limits) alike, and record the holds they passed. A usage window or a checkout occupant still holds them. `restart` and `resume` keep the run's choice. |
| `--at <NODE>` | Start at this check or step instead of the definition's `start`, whether or not a route reaches it. An unknown node refuses with `unknown workflow node <node>` (exit 1) and creates nothing. The run's journal opens with a `start` row naming the node. |
| `--input <OBJECT>` | One JSON object handed to the start node as its `input`, as an answer that routed there would be: it needs a string `outcome`, and its `key`, when present, is a string or an integer and becomes the run's key. Anything else exits 2 naming `--input`. The run's journal opens with a `start` row carrying it. |
| `--once` | Try the start node: the run ends `tried` at that node's first answer that is not an `unknown` wait, whatever the route it would take, and the command waits for it ([trying one node](#trying-one-node)). |
| `--detach` | From an agent: the run owes the agent no [`WORKFLOW_REPORT`](#starters-and-depth) when it ends, and does not hold the agent open while it runs. Join it with [`workflow wait`](#wait-for-runs) instead. Accepted from a shell too, where it changes nothing. |

Before creating a run, the command validates the definition and refuses on any error, printing each as `<workflow>: <path>: <message>`. Warnings alone do not refuse: each prints to stderr as `warning: <workflow>: <path>: <reason>` before starting. It needs an existing room (`rimz start` creates one).

Then, before anything is created, it reads the reporting hooks and folder trust each agent step and judge launch would need, at the run's checkout: every agent step and every judge, or with `--once` the tried node and its rest check. A checkout `-w` would create skips the folder read and keeps the hooks read. A node whose launch cannot be resolved refuses with its error. Any problem refuses (exit 1), listing each distinct problem once with its fix and the nodes it blocks:

```text
error: workflow start refused; nothing was created:
  - <problem and its fix> (blocks <node>, <node>)
```

A loop task's fire refused this way records an errored fire carrying this text and creates no run.

With `scope = "worktree"`, nodes run in the selected checkout. With `scope = "project"`, they run at the project root, even when invoked from an owned worktree. `-w` on a project-scoped workflow refuses before anything is created, with `<workflow> has scope = project, so it runs at <project root>; start it without -w` (exit 1). A loop task's fire into a worktree is not a `-w` start and runs at the project root.

One unfinished run holds each `(workflow, scope path)` pair, and a run a loop task started also holds `(task, scope path)`. A second start refuses and names the existing run, a start given `--at` or `--input` included. A try (`--once`) holds no slot: it starts beside any run, and any run starts beside it. A loop task's tail refuses all three flags, each naming the flag, since a task's run always runs the whole workflow from its start, and refuses `--detach` too, since a task's run owes the task its report. The CLI validates before it asks for that slot, so a broken definition refuses on validation first.

A run records who started it and how deep it sits ([starters and depth](#starters-and-depth)). A start past the depth limit refuses before a worktree, a home, or a record is created.

On success it prints three lines:

1. The run id, printed at once. This line keeps its scripting format.
2. The run's work directory (`RIMZ_WORKFLOW_DIR`) where the run's home is after the first decision: the finished directory when the run ended in it, since an ended run's home moves there. This line keeps its scripting format.
3. `→ rimz workflow show <run>`.

### Starters and depth

A run started from a run's command, script, or check (a process that carries `RIMZ_WORKFLOW_NODE`) is started by that run, at the node it was at, even when the process also carries an agent's variables. Any other start from a living agent is started by that agent, recorded by its launch: a step agent of a running run, a kept step agent whose run moved on or ended, and an agent outside any run. Another start inside a run (one that carries `RIMZ_WORKFLOW_RUN` and resolves no agent) is started by that run. A start by hand, and a loop task's run, have no starter.

The depth of a run started by a run is that run's depth plus one. A run started by an agent sits one deeper than the agent: a workflow step's agent, and its peers and subagents, carry their run's depth (read as 1 on an agent launched before agents carried it), and a run any other agent starts is at depth 1. A run with no starter is at depth 1. A start deeper than `[agents] max-workflow-depth` (default 3, [configuration](../../guide/configuration.md#agent-launch-chains)) refuses with exit 1: ``workflow run refused: it would start at workflow depth <N>, past this machine's `[agents] max-workflow-depth` of <limit>; nothing was created. Do not retry it from here; a person can raise the limit with `rimz config set agents.max-workflow-depth <N>` ``.

A run an agent started without `--detach` owes that agent its outcome report. When the run ends, the agent gets one `WORKFLOW_REPORT` message from `@rimz`, parked for its next turn boundary:

```text
workflow run <run> (<workflow>) ended <done|canceled|failed>
reason <reason>
checkout <path> on branch <branch>
notes <notes folder>
history: rimz workflow show <run> --history
```

The `reason` line appears when the run recorded one, the branch when the checkout is on one, and the `notes` line when the run kept notes. A try that ended `tried` reads `done`. Until the report is queued, the finished run's home stays in the live directory and its paced advance stays due, and the agent is held open and shows the run as a wait. An agent that ended before the run did is sent nothing. A run started by hand, by a loop task, by another run, or with `--detach` sends no report; a run a [runs step](#runs-steps) started stores its ending on the run that started it instead.

### Trying one node

`--once` runs one node for real on the real engine: a command step's command runs and an agent step launches its agent. After the receipt's lines, when the first decision left the try waiting, one more line names the node and the wait in the phrase `show` prints. Then the command blocks, with no time limit, until the try has ended and its run directory has moved to `finished-workflows/`, and prints what `rimz workflow show <run>` prints for the finished run: its history opens with `tried`, holds the node's own rows (the answer with its readings and reason, an attempt), and ends with `terminal tried · stopped after one node; the run would go → <target>`. It exits 0 when the try ended `tried`, and 1 with `workflow run <run> ended <state>, not tried` when it was canceled or failed (a definition the try no longer fits, for example). Interrupting the command leaves the try running; `rimz workflow cancel <run>` ends it.

A try waits through a condition's `unknown` and through capacity, window, budget, trust, and occupancy holds. A session step stays live through a kept verdict and a push-back, and its try ends when the session closes. `resume --at` refuses a try; `extend`, `restart`, and `cancel` work on it. A bare run address selects a real run in the same checkout over a try, so a try's own views name it by id. A try that waits advances as any run does: about once a minute while the room is open, and otherwise only on the [loop timer](../../guide/loops.md#who-keeps-time).

### Workflow times

`[workflow].deadline`, `run --deadline`, `run --give-up`, and `extend --deadline` take one spelling. `workflow validate`, the start's validation, and the flags check it without a clock, and refuse any other value with `"<value>" is not a workflow time; give a duration (8h), HH:MM, <weekday> HH:MM, a date (2026-11-14), a date-time (2026-11-14 09:00), or a timestamp (2026-11-13T21:00:00Z)`.

| Form | Example | Instant |
| --- | --- | --- |
| Duration | `8h`, `90m`, `2d` | That long after the time resolves. Units `s`, `m`, `h`, `d`, at most `365d`. |
| `HH:MM` | `18:00` | The first such time strictly after the time resolves: a time equal to now names tomorrow's. |
| `<weekday> HH:MM` | `fri 18:00` | The first such day and time strictly after the time resolves. A weekday is its name or a short form (`mon`, `tues`, `thurs`), in any case. |
| `YYYY-MM-DD HH:MM`, `YYYY-MM-DDTHH:MM[:SS]` | `2026-11-14 09:00` | As written. |
| `YYYY-MM-DD` | `2026-11-14` | Midnight at the start of that date. |
| RFC 3339 timestamp | `2026-11-13T21:00:00Z` | As written, in its own offset. |

Every form but a duration and a timestamp reads in the top-level [`timezone`](../../guide/configuration.md#sidebar-rendering) setting, or the system zone when it is unset. A time resolves once: a run's deadline and give-up when `run` creates the run, an extend's deadline when the extend runs. An instant already past is accepted, with no rounding. A run keeps the instant it stored, whatever its definition says later.

The first advance starts in the background, and the command waits up to 30 seconds for its first decision before it prints lines 2 and 3; a run that ends in that time is waited for until its advance has moved its home. When the run is held in front of its node by capacity, a usage window, or a checkout occupant, a fourth line names the node and the hold in the phrase `show` prints, such as `build held 0s · claude: no capacity (your check)`. `rimz loop fire` and `rimz workflow restart` add the same line to their receipts. A run that launched, ended, or whose advance exited without deciding prints the three lines alone.

Before it writes anything, `run` opens the home-wide start lock, `~/.rimz/loops/throttle.lock`, for write without taking it, since the background engine opens the same file on its first launch. `resume`, `extend`, and `restart`, including `rimz agents restart` on a workflow agent, do the same. When the open fails, the command exits 1 with `nothing was started` (`resumed`, `extended`, `restarted`) and the cause, and leaves no run, record, or journal change behind. A read-only RimZ home, for example inside a provider's command sandbox, adds `this command cannot write the RimZ home` and the [sandbox remedy](./wait.md#inside-a-providers-command-sandbox), worded for `rimz workflow`. An agent inside its provider's sandbox runs `rimz workflow run` bare, so its [routine rule](../../guide/fleet.md#set-a-permission-mode) runs it outside the sandbox. The check covers RimZ's own files only: a check script that writes elsewhere, such as a tool's cache, can still fail inside a sandbox.

While the room is open, its scheduler advances every due run about once a minute. With no room open for the project, only the [loop timer](../../guide/loops.md#who-keeps-time) does.

### The run directory

`run` creates `~/.rimz/ws/<workspace-dir>/owned/workflows/<run-id>/`. When the run finishes, the directory moves to `owned/finished-workflows/<run-id>/`.

| Entry | Holds |
| --- | --- |
| `record.json` | The run's current position, the truth the engine reads. |
| `journal.jsonl` | The append-only history behind `show --history`. `show --json` prints every entry. |
| `definition/` | A [loop task](#loop-tasks)'s run only: the definition its fire generated, `workflow.toml` and the files beside it. A run of a deployed workflow has none: it reads `~/.rimz/workflows/<name>/` at every step ([live definitions](#live-definitions)). |
| `work/` | The work directory, `RIMZ_WORKFLOW_DIR`. |
| `history.txt` | Written at the end: the colorless `--history` view ([finished runs](#finished-runs)). |

`work/` holds:

| Entry | Holds |
| --- | --- |
| `context.json` | Rewritten before every check and attempt ([the context file](#the-context-file)). |
| `notes/` | A link to the run's kept notes folder, `automation-notes/<yyyymmdd>/<HHMMSS>Z-workflow-<workflow>-<checkout>/` under the room's state directory. |
| `signs/` | One `<step>.md` per agent step that has a sign. |
| `<seq>.check.log` | A test's combined output, or a script's complete stderr (written even when empty). |
| `<seq>.step.log` | A command step's combined output. |
| `<seq>.claim.json` | An agent attempt's claim. |

Every evaluation and attempt takes a new `seq`, a retry included, so a notes write named after the item it records makes a repeat overwrite it. `~/.rimz/workflows/` is never written.

### Live definitions

A run keeps no copy of its definition. Every `workflow advance` loads the definition folder again and validates it as `workflow run` does, so an edit reaches every live run of that workflow at its next step: routes, prompts, scripts, `run` paths, timeouts, and limits are read as they are then. A definition edit cannot change what the run recorded: its scope, its deadline, and its visit grants. Only [`extend`](#extend) changes the deadline and the grants of a live run. Agent profiles and machine configuration are read at each step and judge launch.

| The definition | What the run does |
| --- | --- |
| Cannot be read, does not parse, or validation refuses it | The run waits where it is. `workflow advance` exits non-zero and changes nothing, and the next advance after the fix proceeds. A check or command step already running settles under the definition it started with, and the run waits at its next move. `list` and `show` print `waits on its definition: <first error>`. A loop task's run whose generated definition no longer parses ends `failed` instead, with `this RimZ cannot read the definition generated at the fire: <error>`. |
| No longer fits the run | The run ends `failed` and takes no route. Its attempt or session is stopped, and its reason is `<node>: the workflow definition changed under the run: <what>`. |

A run no longer fits when its current node is gone, or a live attempt's node is no longer the same class: an agent step, a message step, a command step, a judge, or another check (a test turned into a script still fits). It also no longer fits when an attempted agent step has another `lifecycle`, a held session's or a pending push-back's step lost its rest checks, a held session's judging check left the step's `rest-check` list, a staged `asked` route is gone, or a held session's verdict waits in a `for` hold on a case its condition no longer has. A folder moved or deleted after the start counts as missing. A symlink to the folder is resolved when the run starts, so pointing the link elsewhere changes nothing for that run. [`cancel`](#cancel) works whatever state the definition is in.

### Environment

Workflow agents, judges, tests, scripts, and commands receive:

| Variable | Value |
| --- | --- |
| `RIMZ_WORKFLOW_RUN` | The run id. |
| `RIMZ_WORKFLOW_DIR` | The work directory. |
| `RIMZ_WORKFLOW_DEFINITION` | The absolute path of the run's definition folder: the deployed `~/.rimz/workflows/<name>/`, shared with every run of that workflow, or a loop task's run's own `definition/`. |

Every workflow agent's launch Environment section has a `- workflow:` line naming the definition folder, even with `env_reminder` off. Non-workflow agent launches unset all three variables. Tests, scripts, and commands also receive `RIMZ_WORKFLOW_NODE`, the node they run at, which every agent launch unsets, so a `rimz workflow run` from one of them names the run as its starter ([starters and depth](#starters-and-depth)). If the agent launch cannot read the run record, it warns and omits the definition variable and line. Tests, scripts, and commands also inherit the room's workspace environment.

## Definition fields

A definition is `workflow.toml`: one `[workflow]` table and any number of nodes, each a table named by its kind, `[<kind>.<name>]`. A name is unique across all nine kinds, and takes ASCII letters, digits, `-`, and `_` only. The workflow's folder name follows the same rule. A field marked required refuses when absent. A field a kind's table does not list refuses there ([validation](#validate-and-schema)). Every node also takes `summary`.

| `[workflow]` | Meaning |
| --- | --- |
| `scope` | Required. `worktree` or `project`. |
| `start` | Required. The first node: a check or a step, never an `@` target. |
| `summary` | One nonempty line describing the workflow. |
| `deadline` | A [workflow time](#workflow-times) that bounds the run, resolved when the run is created; a duration counts from then. `run --deadline` replaces it. |
| `limit` | `max-parallel` and `stagger`, at least one ([limits](#limits-and-capacity)). |
| `on.failed` | Where a failure goes when its node routes neither its cause nor `on.failed` ([failures](#failures)). |

| `[agent.<name>]` | Meaning |
| --- | --- |
| `agent` | Required. A kind, profile, virtual cell, or layout; a team only in a session with no `rest-check`. |
| `prompt-file` | Required. A file confined to the definition directory. |
| `then` | Required. One check, `@done`, or `@failed`: where the run goes when the attempt ends on its own. |
| `lifecycle` | Required. `turn` or `session`. |
| `rest-check` | A session only. A nonempty list of checks run at each clean rest. |
| `timeout` | The attempt's bound from its launch, on either lifecycle. |
| `max-visits` | At least 1. |
| `keep` | Either lifecycle. Default false. |
| `notes` | `false` omits the notes instruction. Default true. |
| `subscribe` | Signals forwarded to a session's leader. A `turn` step refuses it. |
| `on.asked` | Where a blocking question goes (either lifecycle). |
| `on.errored` | Where the leader's own errored turn goes (either lifecycle): a turn end the provider failed, with an error waiting will not clear ([below](#how-an-attempt-ends)). |
| `on.died`, `on.timeout`, `on.visits`, `on.capacity`, `on.deadline`, `on.failed` | Failure routes. |
| `admission` | `window`, `occupancy`, `max-parallel`, `stagger`, `throttle`, `give-up` ([admission](#admission)). |
| `account`, `mode`, `effort`, `fast`, `budget`, `system-prompt-file`, `worktree` | Launch settings ([launch settings](#launch-settings)). |

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
| `timeout` | Required. |
| `wake` | Signals that re-run the test. |

| `[script.<name>]` | Meaning |
| --- | --- |
| `run` or `shell` | Required, exactly one. Stdout is the JSON answer. |
| `on.<outcome>` | One target per outcome the script answers, plus the failure routes `on.died`, `on.invalid`, `on.timeout`, `on.deadline`, and `on.failed`. |
| `timeout` | Required. |
| `wake` | Signals that re-run the script. |

| `[judge.<name>]` | Meaning |
| --- | --- |
| `agent` | Required. An agent kind or profile. |
| `prompt-file` | Required. A file confined to the definition directory. |
| `summary` | Required. |
| `on.<outcome>` | One target per outcome the judge answers, plus the failure routes `on.died`, `on.invalid`, `on.timeout`, `on.errored`, `on.deadline`, and `on.failed`. `on.asked` refuses as a dead route: a question never ends a judge. |
| `timeout` | Required. Runs from the judge's launch and never pauses. |
| `wake` | Signals that re-run the judge. |
| `admission` | `window` only. |

| `[condition.<name>]` | Meaning |
| --- | --- |
| `cases` | Required, nonempty. Each case takes `when` and `outcome`, both required, and `for`. |
| `else` | Required. The outcome when no case is true. |
| `on.<outcome>` | Required for every case outcome and `else`. |
| `on.unknown` | Required. |
| `on.timeout`, `on.deadline`, `on.failed` | Failure routes. |
| `timeout` | Required. |
| `provider`, `account` | The window terms' agent kind and login. |

| `[sleep.<name>]` | Meaning |
| --- | --- |
| `sleep` | Required. A nonzero duration. |
| `then` | Required. A node, `@done`, or `@failed`. |
| `when`, `for` | An early end: an ANDed term array, and how long it must hold. |
| `provider`, `account` | The window terms' agent kind and login. |
| `on.deadline`, `on.failed` | Failure routes. |

Durations (`timeout`, `sleep`, `for`, `stagger`) take `s`, `m`, `h`, or `d`, at most `365d`. `admission.give-up` takes the same units with no maximum, or `forever`.

A `summary` is one nonempty line, not instructions to the runner. A blank or multiline summary refuses at its path. A judge without one refuses at `judge.<name>.summary` with `add summary = "..."`. `workflow run` checks this whether you or a loop task starts the run.

### Routes

Every work node requires `then`: for an agent or a runs step one check, `@done`, or `@failed`; for a command one check; for a message exactly one node, `@done`, or `@failed`. A list refuses at `<kind>.<step>.then`. A check's `on.<outcome>` is a target string: a node, `@done`, `@failed`, or `@wait`. A table or any other value refuses. A failure route cannot target `@wait`. `@break` and `@user` refuse as targets anywhere (`<path>: @break is not a route target; route to a node, @done, or @failed`).

A step at the start runs through the same admission as any later visit. Its first `context.json` has `input`, `key`, and `prev_attempt` set to null (`input` and `key` from `run --input` when given), with `visit` and `visits` both 1.

### Agent steps

An agent step requires `prompt-file` and `lifecycle`. A missing `lifecycle`, or any value other than `turn` or `session`, refuses at `agent.<step>.lifecycle`.

`agent` accepts the fresh-launch layout grammar of `rimz agents`: a kind, profile, virtual cell such as `claude-auto`, or a layout such as `claude,nvim` or `yagni,reflect-notes`, with agent and command cells. It needs an unambiguous prompt leader. Command-only layouts and ambiguous prompt leaders refuse with a field-specific fix. A session without a `rest-check` may name a team: it launches the team's cohort as `rimz teams launch` does, its leader as the prompt leader. A team on a `turn` step, or on a session with a `rest-check`, refuses with `a team runs only in a session with no rest-check; set lifecycle = "session" and remove rest-check`.

The layout opens in an unfocused new tab titled `run <leader kind>`. Every cell is pinned to the run and gets the workflow environment. Only the leader gets the composed prompt and owns the [supervised run](../../guide/scripting.md#what-a-run-does-on-your-machine). The leader's reporting hooks must be installed and trusted, its program on `PATH`, and its account must trust the checkout. `workflow run` reads the hooks and the folder trust for every agent step and judge before it creates anything ([run a workflow](#run-a-workflow)). When one fails later, at a step or judge launch, and the user can fix it in place (hooks missing, disabled, or untrusted, or a folder undecided or saved untrusted), the run withdraws the attempt, counts no visit, and waits `trust` with each fix in its detail. Each advance reads again, the first after the fix launches, and only the run deadline ends the wait, or, for a rest-check judge, its held session's timeout. A session leader of a loop task's run, and a team step's leader, take the companions' layout checks in place of these. An unset leader permission mode defaults to `auto`. Companions use the ordinary layout checks: an undecided folder warns, while a saved-untrusted folder or untrusted hooks refuse the launch. The room's turn, room, and account budgets apply to every cell. Attempt cost counts the leader only, and companion spend appears on their cards and in stats.

The leader's prompt is the prompt file, then `signs/<step>.md` when that file exists, then the run's `--prompt`, then, in a run a loop task's fire started, the fire's evidence line (the `RIMZ_LOOP_TRIGGER` its commands read) when the fire carried one. RimZ does not expand variables in it.

#### The launch reminder

Only the leader gets a `### Workflow` launch section. It holds what a workflow is, a compact graph rail marking this step, who started the run (automatic runs name the loop task and its current trigger when available), why it arrived here, the visit caps that apply and that push-backs count as visits, the previous attempt, the step's working-memory file, pointers to `rimz workflow show` and `rimz workflow show --history`, and what follows its turn. It describes each rest check in order, then the `then` check:

- A `run` check shows the command as written, any summary, and the rule that slash-containing words name files inside the definition folder while the command runs from the checkout.
- A `shell` check shows the command and any summary, says it runs through `sh -c` from the checkout, and names `$RIMZ_WORKFLOW_DEFINITION` for scripts in the definition folder.
- In both cases the step agent may run the command itself.
- A condition shows its ordered cases.
- A judge shows only its summary, never its prompt filename.

`rimz workflow restart`, and `rimz agents restart` on a step agent, compose a new note saying the user restarted the step. Ordinary agent resume refuses a current workflow step, and room rebirth leaves its recovery to the workflow. Providers without a system-text channel receive no launch reminder. A session's note also lists its subscriptions with their resolved scope.

#### Launch settings

An agent step's launch settings apply to each fresh launch as the matching `rimz agents` flags would:

| Field | Meaning |
| --- | --- |
| `account` | A login name, pinned on every cell. The start throttle counts the launch in that account's start group and skips availability routing. |
| `mode` | `auto`, `ask`, or `yolo`. `plan` refuses. |
| `effort` | The effort level. |
| `fast` | `true` or `false`. |
| `budget` | The `--budget` grammar, such as `"$5"` or `"$20/day"`. |
| `system-prompt-file` | A file confined to the definition directory, refused at `agent.<step>.system-prompt-file` otherwise. |
| `worktree` | A worktree name the launch creates or reuses, empty for a generated name. The agent starts there instead of the run's checkout. |

A restart that resumes a session ignores `account`, `system-prompt-file`, and `worktree`, and applies the rest. It runs in the run's checkout, on the room's current account for the agent's kind when that account shares the session's history, and refuses otherwise.

#### How an attempt ends

Only a clean end follows `then`. A blocking ask with `on.asked` set, and an errored turn with `on.errored` set, follow their own edge. Every other ending is a [failure](#failures).

| Lifecycle | Clean end | `died` |
| --- | --- | --- |
| `turn` | The leader's first clean turn end. RimZ then stops every cell and its subagents, closes the command cells, records the commit before and after, and follows `then`. | A provider exit before the clean turn end. |
| `session` | The leader's provider process exits at rest (below). Every cell closes and the run follows `then`. | Any other exit: before registration or the first turn, mid-turn, with a blocking ask open, after an errored turn, or with background work, a pending `rimz wait`, or a budget park on its card. |

A session's exit is at rest when the leader had ended a turn and has no turn open, no blocking ask, no errored turn end, no background work or pending `rimz wait`, and no budget park on its card. A message still waiting for the leader, or a child's result it has not collected, does not count against an exit at rest. A session never reads `then` at a turn end. A molt of the leader keeps the step: the replaced session's exit ends nothing, and the exit of the new session decides.

On both lifecycles, failed or interrupted turns, live provider limits, and budget parks keep the agent open, and a later clean turn completes the attempt. Some providers report an interrupt as a clean end, and RimZ follows that report. The one exception is an errored turn on a step that sets `on.errored`: the leader's own turn ended failed, and its card shows a provider error, an unknown error, or a lapsed plan. The attempt then ends `errored` and routes `on.errored` with the input `{"outcome": "errored", "reason": <the error>, "errored": {"text", "class", "handle"}}` and the run's key. A blocking ask wins when the step routes both. A rate or spend limit or an overload never ends the attempt, nor does a companion's error. A Claude Console API usage cap (`You have reached your specified API usage limits`) leaves the leader's card reading `failed`, ends the attempt through `on.errored` when the step sets it, and otherwise keeps the step open. A budget that would refuse the launch holds the step before it, as the [`budget`](#the-waiting-line) wait, with no attempt or visit counted. A companion's question or limit does not end the leader's attempt. A peer or team launched later by a cell keeps running until stopped.

`workflow list` reads a live session as `with you in @<handle>`, and `show` as `<step> is with you`, or `<step> is asking you` while a blocking ask is open. A step of either lifecycle held on a stop reads `<handle> held <age>: <stop>` in `list` and `<step> is held <age> · <handle>: <stop>` in `show`, where the stop is `provider error`, `unknown error`, or `plan lapsed` (an errored turn, with its text when known), `rate limit`, `spend limit`, `provider overloaded`, or `budget park`. The leader's own errored turn names the hold over a companion's stop.

#### Timeouts and asks

A step's `timeout` bounds its attempt from launch, less the time its leader, or a subagent it launched, spends held on a blocking question, permission prompt, provider limit, error, or budget park. Overlapping holds count once. Silence and interrupts do not pause it, and the run deadline never pauses. The timeout ends the attempt as `timeout` and fails the step with cause `timeout`. A [`workflow restart`](#restart) counts it again from the relaunch.

A blocking question or permission prompt from the leader or its subagent keeps the attempt live on either lifecycle. A subagent's ask or answer is noticed within about a minute. With `on.asked` set, RimZ instead stops the layout (or leaves it open outside the run under `keep = true`) and follows that edge, with the question in `prev_attempt.asked`. Only blocking native asks pause an attempt. Async questions and explicitly raised questions remain actionable in `rimz asks` but do not pause the timeout.

#### Rest checks

`rest-check = ["tests", "review"]` on a session names the checks that judge its clean rests. Each entry must name a test, script, judge, or condition.

| Refusal | Message |
| --- | --- |
| An empty list | `rest-check must list at least one check` |
| A step, a sleep, an `@` target, or an unknown name | `rest-check lists only check nodes (test, script, judge, condition); <entry> is not one` |
| `rest-check` on a turn step | `a turn is judged at its turn end; rest-check is a session's field` |

At each clean leader turn end, with the leader at rest and no blocking ask, provider limit, error, interrupt, or budget park open, the run holds the session and enters the first listed check. Where the answer routes decides:

| Route | Effect |
| --- | --- |
| Back to the step | The answer becomes the next prompt in the same session, updates input and key, and spends one visit. A push-back past the cap fails the step with cause `visits` instead, closing the agent by default (detaching it under `keep`). |
| `@wait` | Holds the session at the check without spending a visit. Subscribed signals and the minute tick re-sample it while the leader stays at rest on that turn. |
| Another listed check, a failure route included | Keeps the session open and starts the destination check's clock. An answer keeps the step's input and key, so a later check reads earlier answers from their output files or the world. A failure hands the next check its failure input. |
| Anything else | Closes the session, closing the layout by default, and the run goes there. |

The push-back prompt names the check that answered, its outcome, its `reason` when present, then, for a test or script, `output: <path> (<size label>)`: a test's or script's `<seq>.check.log`. An empty or missing file reads `no output`. A judge's push-back carries no output line: its answer is the whole account. No tail or file content is copied into the prompt. A condition push-back explains its false terms and unread readings instead. The prompt says the run moves on when the check passes.

A new leader turn abandons a held wait and any check in flight without stopping the session, and discards a saved closing verdict. The next eligible turn end starts again at the first check. Provider exit, deadline, cancel, and restart end the session as they do without a rest check: an exit at rest follows `then` and runs no rest check first.

The step's `timeout` keeps running while the session rests at its check, beside the check's own `timeout`. Each fails its own node: the session's abandons the check and fails the step, the check's fails the check. When both have passed by the time RimZ looks, the session's wins, and a `@wait` at the check sleeps no later than the session's timeout.

While a rest check runs, `list` reads `with you · checking <check>`, with any waiting detail appended. A blocking ask takes precedence. Each listed check is an edge of the step with outcome `rest-check`, so `validate` lists it, a rest check that routes back to its session is a cycle that needs `max-visits`, and a listed check is never unreachable. A listed check reached without a held session routes normally.

#### Keep

`keep = true` on an agent step of either lifecycle leaves the leader and companions open, outside the workflow, on a natural attempt end, an `on.asked` or `on.errored` ending, or a closing rest-check route (a rest check's failure route included). The workflow no longer owns them. A forced ending (cancel, restart, `resume --at`, the step's `timeout`, the deadline) and a `died` or `canceled` attempt still stop them. A push-back past `max-visits` detaches them. On a `turn` step, a check returning to the step can leave the old agent beside the new launch, and occupancy still applies.

#### Notes

Agent steps default to notes on. Their launch reminder names `<step>-notes.md` in the run's kept notes folder as working memory across visits, restarts, and subagents, including a record of changes outside the repository. Only that step's agent and its subagents are told to write it. `notes = false` omits the instruction. Only an agent takes `notes`. Runs created before notes support keep their old scratch directory and receive no notes instruction. RimZ never reads the notes. `gc.automation_notes_retention` defaults to keeping closed notes folders forever, independently of run-directory cleanup.

#### Admission

An agent step does not start until its admission passes, in this order: its usage window, the checkout's occupants, the workflow and step limits, then the machine start throttle. Command steps, checks, and an already-open session never ask for admission.

| `admission` field | Meaning |
| --- | --- |
| `window` | A usage-window term such as `5h.left>=40` ([window terms](#window-terms)). Holds until every agent cell's provider has room. |
| `occupancy` | `wait` (default): wait until no agent occupies the checkout. `takeover`: stop an idle occupant, and wait for one mid-turn. `share`: launch beside any occupant and stop nobody. Agents in other RimZ worktrees do not count, and neither does the agent that started the run while the run owes it its report: the run never waits on it, and a takeover never stops it. A sharing agent still occupies the checkout for later steps. |
| `max-parallel`, `stagger` | The step's own [limit](#limits-and-capacity). |
| `throttle` | `off` skips the machine turn, stagger, resource limits, and capacity check, never the machine caps or the workflow and step limits. |
| `give-up` | A positive duration, measured from the first capacity hold, or `forever`. Unset, the run's `--give-up` instant bounds the wait, and without one the step waits until the run is canceled. |

### Command steps

A command step runs `run` or `shell` in the checkout, in a session and process group of its own with no controlling terminal, with stdin closed, the room's workspace environment, and the [workflow environment](#environment). Both output streams go to `work/<seq>.step.log`. It opens no pane, waits for no occupant or capacity, and takes no prompt: signs, `--prompt`, and a loop fire's evidence line compose only agent prompts and message-step messages, and a command reads `context.json` and `RIMZ_LOOP_TRIGGER` instead.

On a `command`, `test`, or `script`:

- `run` is a command line split into words and passed literally, without a shell. A quoted word with spaces stays one argument. A slash-containing word, data argument or not, must name an existing regular file inside the definition directory, given as a relative path without a leading `./` or any `..`. A link must resolve inside the directory. A word without a slash is passed as is, so `sh check.sh` reads `check.sh` from the checkout and `sh checks/check.sh` reads it from the definition.
- `shell` runs through `sh -c`, without path confinement.

Which one a node sets decides only how its command starts.

| Ending | Records | Then |
| --- | --- | --- |
| Exit 0 | `ended` | Follows `then`, with the original routed input unchanged and the outcome in `prev_attempt`. |
| Another exit, a signal, a spawn failure, or a log-open failure | `died` with a reason | Fails with cause `died`. |
| The step timeout | `timeout` | Stops the group and fails with cause `timeout`. |
| The run deadline | `canceled` | Stops the group and fails with cause `deadline`. |
| `workflow cancel` | `canceled` | Stops the group and ends the run. |
| A lost host | `died`, reason `lost command-step host <pid>` | The next advance stops any survivor and fails the step, without rerunning the action. |
| An interrupted command whose bound is no longer due | `canceled` | Fails with cause `died`. |

A child that exited on its own keeps its exit outcome even if a bound fell due before settlement. The failure's `detail` carries the exit `code` (absent after a signal or a spawn error) and the `log` path when the log exists.

### Message steps

A message step's `message` is an agent address, `@<handle>` or `@<handle>#<channel>`. `@user`, `@me`, a bare name, and a pane id refuse at `message.<name>.message`. `prompt-file` is optional and confined. A message never reads occupancy, windows, capacity, or the start throttle.

Each visit composes a header line naming the workflow, step, and run, followed by the prompt file, signs, run prompt, and the loop fire's evidence line, as an agent step's prompt is composed. Without any of them the message is the header alone. It resolves the address as `rimz message` does, from the run's checkout among living agents, and queues one automated `Type: WAIT` message from `@rimz` at gate `done`, sent at once to a resting agent. It records `ended` with the target's handle and follows `then` once queued, whether the agent is mid-turn or resting.

- No single match, or a prompt that cannot be composed, records `died` with the reason and fails the step with cause `undelivered`.
- `session = "<agent session id>"` pins the step to one agent. A later session of that agent, after a restart or a molt to any provider, passes. When the address resolves to another agent (a handle since reused), the visit records `died` with `<address> is session <id>, not the pinned session <pin>` and fails with cause `undelivered`. A blank `session` refuses.
- A lost host sends the same message id again on the next advance, and the queue keeps one copy and its first target.
- A cancel, deadline, bound, or `resume --at` that ends the attempt before it settles retires the message if it is still queued. A resting target may already have received it.

A message step returns at once, so a cycle through it needs `max-visits` and a waiting check.

### Runs steps

A runs step, `[runs.<name>]`, starts other deployed workflows and waits until each has ended. It takes:

| Field | Rule |
| --- | --- |
| `start` | Required. A nonempty array of `{workflow, prompt-file}` tables: `workflow` names a workflow deployed under `~/.rimz/workflows/`, and `prompt-file` is optional and confined. An entry takes no other key. |
| `prompt-file` | Optional and confined. Read for every started run. |
| `timeout` | Optional. Fails the step with cause `timeout`. |
| `then` | Required. One check, `@done`, or `@failed`. |
| `max-visits` | Optional. |
| `on` | The routes for `died`, `timeout`, `visits`, `deadline`, and `failed`. |

Each visit starts one run per entry, in order, each in a new worktree of the project cut from the commit the run's checkout is at, so every started run begins from the same commit. A started run's prompt is the step's `prompt-file`, the entry's `prompt-file`, the step's sign, then the run's `--prompt`, each when present. A loop fire's evidence line is not included. Each started run is started by this run at this step ([starters and depth](#starters-and-depth)) and sends this run no `WORKFLOW_REPORT`: its ending is stored on this run's record instead.

Before it starts any run, the step fails with cause `died` when:

- the run's checkout has no commit, or has uncommitted changes, untracked files included, which the new worktrees would not see;
- the started runs would sit past `[agents] max-workflow-depth`;
- an entry names a workflow that is not deployed, that `workflow run` would refuse, or that has `scope = "project"`, which gets no worktree of its own.

A worktree or run that cannot be created after the first run started also fails the step `died`, and the runs already started keep running.

Once every started run has ended, the step follows `then` when each ended `done`, and otherwise fails with cause `died`, its reason naming each run that did not (`started run <id> (<workflow>) ended <state>[: <reason>]`, joined by `; `). Until then the run waits as `runs`, shown as `runs: <n> of <total> started runs ended`. The next step's `context.json` carries `joined`, one ending per started run, and its launch reminder lists each with its `workflow show` command.

A timeout, the run deadline, a `resume --at`, and a `workflow cancel` end the step's waiting and leave its started runs running. `validate` warns, without refusing, about an entry that a start would refuse as not deployed or project-scoped, and about a workflow that starts itself again through runs steps. `workflow run` and `loop add --workflow` print the same warnings to stderr before they go on.

### Checks

Every test, script, judge, and condition requires a `timeout`. The timeout starts each time the run enters the check and runs across every evaluation and wait there. A judge's runs from its launch instead, and nothing it is stopped on pauses it. A test does not inherit the loop's default timeout. When it passes, the check fails with cause `timeout`.

Tests and scripts run as child processes in the checkout with no stdin, in a session of their own detached from any terminal, so a command that opens `/dev/tty` cannot stop on job control. Timeout, deadline, and cancellation stop the whole process group. An interrupted check can run again: the process hosting it can die after the command ran.

| Kind | Answer |
| --- | --- |
| `test` | Exit 0 answers `pass`, any other exit `fail`. A signal, a spawn error, or a log-open error raises `died`. Both streams go to `<seq>.check.log`. Requires both `on.pass` and `on.fail`. |
| `script` | Stdout is one JSON object ([the check answer](#the-check-answer)). It must also exit 0: a nonzero exit or signal raises `died`. Its complete stderr goes to `<seq>.check.log`, even when empty. |
| `judge` | An agent in a pane answers through [`rimz workflow submit`](#workflow-submit). |
| `condition` | Ordered cases over [condition keys](#conditions), with no child process. |

Each evaluation launches a fresh agent in a pane of the run's checkout, with the Judge role and a session lifecycle. Its permission mode is automatic unless its profile sets one. Its shorter `### Workflow` note names its check, the graph position, and the run's origin, says its answer chooses where the run goes, and points to `rimz workflow show $RIMZ_WORKFLOW_RUN --history`. Its prompt teaches `rimz workflow submit`. Under scoped isolation, RimZ mounts the checkout and its Git directories read-only for the judge. Under host isolation only the prompt says so. Any agent kind can judge.

- An accepted answer closes the pane and routes the run. A judge's final message is not read.
- A judge at a clean rest without an answer is reminded once. One that exits without an answer fails with `died`; an answer accepted before the exit wins.
- A question or permission prompt, from the judge or a subagent it launched, leaves the judge in its pane: an answer there followed by a `submit` routes as usual, and otherwise the judge fails with `timeout`. A provider limit, an error that waiting clears, or a budget park holds it there the same way, and it resumes and submits in time or fails with `timeout`. `workflow list` and `workflow show` name what it is stopped on, but its timeout keeps running.
- The judge's own turn ending on a provider error that waiting will not clear (its card shows a provider error, an unknown error, or a lapsed plan) fails it at once with `errored`. A timeout that passes at the same look wins over the error.
- A budget that would refuse the judge's launch holds it at its node as the [`budget`](#the-waiting-line) wait, and a trust refusal the user can fix in place as the `trust` wait. Any other launch failure fails it with `died` at once.
- The judge's timeout starts at its launch, so a wait for its `admission.window` or its [`budget`](#the-waiting-line) spends none of it. The deadline wins a tie with an answer.
- A `submit` at or after the judge's timeout is refused, so a late answer never settles the check: it fails with `timeout`.
- An [unsupported agent version](../agent-support.md) refuses the launch, which fails with `died`. `workflow show` includes the upgrade instruction.

### Conditions

`cases` is an ordered list. Each `when` array is ANDed, and its strings take the loop grammar (`!`, `&&`, `||`, parentheses, and branch selectors) over the [condition keys](./loop.md#conditions). The first true case wins, and no true case answers `else`.

- `team.stage` values are checked against the project's effective teams plus `Done`. Validation outside a workspace warns that stage names were not checked.
- Every case outcome, `else`, and `unknown` needs an `on` edge. A case that writes `<key>=unknown` refuses: unread terms answer `unknown` by themselves.
- An unread case before the first true case answers `unknown`, even under `!`. A fresh probe that finds nothing is a reading: with no PR, `pr` reads `none` and `!pr=open` is true.
- `unknown` routed to `@wait` waits for evidence. At a rest check it holds the session and re-samples on subscribed signals and the minute tick while the leader stays at rest. Another target leaves the check.
- An evaluation error, as opposed to an unread term, ends the run `failed` with the error as its reason and takes no route.
- Condition answers inherit the run's key.

**Freshness.** An ordinary check needs a successful forge probe at or after its entry (including after resume). A rest check accepts a probe at or after the session's start, including one from an earlier turn, and samples the latest probe at each eligible turn end. Until a probe meets that floor, the key reads unread and `show` names it. Stage and usage-window readings have no freshness floor. An idle repository's next probe can take up to five minutes, so two consecutive ordinary forge checks each wait for a post-entry probe: put related decisions in cases of one check. Closing the room leaves its last facts that pass the floor in force.

**The `ci` commit fence.** On an open GitHub PR, when the commits are known, bare `ci` also requires CI for the PR's head and the checkout's current HEAD to be on that PR. A local commit not yet on the PR, or a verdict for another PR head, makes `ci` unread, and `show` says which commits differ ([reason forms](#condition-answers)). Someone else's push can count even when the checkout has not fetched it. Missing commit data skips only the comparison it prevents. Without a known PR head, the time floor alone applies, as it does for `pr`, `pr.queue`, `pr.mergeable`, `pr.review`, branch selectors such as `ci[main]`, merged or closed PRs, Gitea, and PR-less checkouts. Loop conditions use the same fence and hold instead of answering `unknown` ([forge freshness](./loop.md#forge-freshness)).

**Holds.** A case's optional `for` is a nonzero duration. A held case must route its outcome away from `@wait`, or its `for` refuses. While the case keeps winning, the run shows `holding red 1m/2m`, and the next evaluation at or after the duration follows its route. A different answer closes the hold, and a later true starts a new one. An `unknown` answer routed to `@wait` keeps a running hold: the same case reading true again continues it from its first true answer. While the answer stays unknown, `show` reads the check as unknown and forecasts no route. The hold ends when its case reads false, another case answers, or a route leaves the check. Changes entirely between two samples go unseen.

**Rechecks.** A waiting check without `recheck_at` runs again about once a minute. A signal matching the current check's subscription clears `recheck_at` and runs it at once. A check with no subscription gets only its recheck time and the minute tick.

#### Window terms

A condition or sleep reading `window.*` requires `provider` (an agent kind, not a profile) and accepts an optional `account`. An unknown provider, a `provider` without a window term, an `account` without a provider, and a window term without a provider all refuse. `admission.window` takes `5h.left` and `7d.left`, the percent left in the 5-hour and 7-day windows, compared with a whole percent, as in loop conditions.

### Wake and subscribe

A condition derives its signal subscriptions from all its cases, including negated terms, and takes no `wake`:

| Term | Subscription |
| --- | --- |
| `ci` | `ci.*` for the run's checkout |
| `pr`, `pr.queue`, `pr.mergeable`, `pr.review` | `pr.*` for the run's checkout |
| `team.stage` | `team.stage` for the run's board |
| `team.stage[branch]` | `team.stage` for any board |
| Forge branch-selector terms such as `ci[main]`, and `window.*` | None: recheck time and the minute tick only |

Tests, scripts, and judges take an optional `wake` list. A conditional sleep derives subscriptions as a condition does. A signal never wakes a step.

Each `wake` entry is a selector string or a table with a required string `signal` and optional `match`. Any other key refuses. Selectors and match values use the [loop signal grammar](./loop.md#signals). Defaults:

| Family | Default scope | Overridden by matching |
| --- | --- | --- |
| `ci`, `pr` | The run's checkout `path` (the project root for project-scoped runs) | `path` or `branch` |
| `team.stage` | The run's current board. Relinking the checkout changes which board wakes it. | `team`, `instance`, or `board` |
| Any other family, such as `review.ready` | Room-wide | Any match filter |

`{ signal = "ci.failed", match = { path = "*" } }` accepts any checkout. An invalid selector or match, an agent lifecycle selector, a team selector other than exact `team.stage`, or a `prompt` on a check's `wake`, refuses at that entry's path, such as `script.review.wake.1`.

A session's `subscribe` takes `wake` entries, with the same defaults read for the checkout its leader launched in (its `worktree` when set). It also takes the team lifecycle signals (`team.idle`, `team.waiting`, `team.failed`, `team.ended`), which default to the team's cohort in that checkout unless the entry matches `team` or `instance`, and `agent.*` selectors whose `match` names another agent by `handle` or `session`. The leader's own `agent.*` signals never reach it. A table entry may add `prompt = "..."`.

- Each matching signal is queued for the leader as a `Type: SIGNAL` message from `@rimz` at its next turn boundary, sent at once when it rests. The message opens with the signal's subject and details, then names the workflow, step, and run, then the entry's `prompt` after a blank line.
- Each entry that matches sends its own message. Entries with the same signal and `match` send one.
- A forward changes no run state, and a check subscribed to the same signal still wakes.
- A run's own [`workflow.*` signals](#run-signals) never reach its leader.
- Forwarding stops when the attempt ends. A forward still queued then is canceled at delivery, and a `keep = true` agent that lives on receives nothing more.
- The launch reminder lists the subscriptions, and `--history` prints one `signal` row per forward.
- A `turn` step refuses `subscribe` with `only a session receives signals; a turn ends where they would land`.

### Sleeps

A sleep takes required `sleep` (a nonzero duration) and `then` (a node, `@done`, or `@failed`, never `@wait` or `@user`), and optional `when` (a nonempty ANDed loop-condition array) and `for` (a continuous hold, nonzero and shorter than `sleep`, which needs `when`). It may take `provider` and `account` for window terms. It takes no `wake` or `timeout`.

- Expiry follows `then`, even during a hold. A true `when` follows it early. A conditional sleep re-samples about once a minute and on its derived signals, so it can end about a minute late.
- Unread data never wakes it, including under negation. Forge readings must be probed at or after the sleep's entry.
- Re-entry or a parked resume restarts its clock. A plain sleep does not evaluate anything between entry and its end.
- A sleep that completes forwards its incoming input and key, and counts no visit.
- Its one failure, `deadline`, hands its route the [failure input](#the-failure-input). An error evaluating `when` ends the run `failed` with no route.
- A `rest-check` cannot name a sleep.

### Limits and capacity

`[workflow.limit]` limits the open agent steps of all runs of that workflow name, across rooms, and refuses with `set max-parallel or stagger` when it has neither field below. An agent's `[agent.<name>.admission]` takes the same two fields and limits that step's starts. An unknown key, an invalid value, or either field on any kind but an agent refuses:

| Field | Rule |
| --- | --- |
| `max-parallel` | Integer at least 1. Counts one per unfinished run whose record holds an agent step attempt, including an idle session, a session held at its rest check (counted under its own step), and an attempt whose agent has not registered yet or has died, until an advance settles it. Companions, RimZ-launched children and peers, judges, and a kept agent whose attempt has ended do not count. A step with companions counts as one, so a layout wider than the limit still starts. |
| `stagger` | Positive duration. Minimum gap between launches at this scope, stamped at launch and shared across rooms and restarts. It is not a gap between admissions or user turns. |

Limits use each run's definition as it is now, so an edit reaches waiting runs at their next retry. They count runs of that workflow name, so a finished run frees its slot even when it kept its agents open. Waiting, paused, hibernated, and compacting sessions count while their run holds the attempt, and so does a parked run's. The incoming start counts as one, and a same-workflow start admitted but not yet launched holds the next one until its launch is reported. Limits apply after window and occupancy, before the two machine caps and other machine checks, even with no `[throttle]` table. No workflow or step limit bypasses a machine cap. Earlier active capacity waiters held by a workflow limit go first across all steps. A step-limit waiter holds only its step. Ties use run id. A freed slot waits for the earlier run's next retry. Limits never stop an existing turn or pane.

`stagger` cannot guarantee the usage window has caught up with the last start's spend: direct usage probes refresh no more than once every five minutes, while a live Claude statusline can update on the next heavy refresh while a sidebar is subscribed. The next start uses the window reading available when it asks.

Agent steps ask the [start throttle](../../guide/configuration.md#agent-start-limits) before taking over a checkout, whatever started the run: `workflow run` (typed by hand or run by a loop task's command) or a `restart`. A held step spends no visit and retries about once a minute. Giving up fails the step with cause `capacity`, for machine and workflow limits alike, and its route, after the [chain](#failures), may not lead back to the step. A window or occupancy wait restarts the capacity clock. The run deadline still applies. When the machine's start queue stays busy for 30 seconds, a step waits as `held` with that reason, uses no visit, and retries on the next tick, unless it sets `admission.throttle = "off"` with no machine cap and no limit. A step already held for capacity keeps its reason and its place in line.

A run started with `--skip-capacity` launches past each hold instead: it claims any applicable workflow limit, counts afterwards, restarts machine, workflow, and step stagger, and records the first hold of each stage it passed (limit, queue, machine, capacity check). `workflow show` prints them as `<holds>; started anyway: the run skips capacity` ([inspect runs](#workflow-show)).

Limit holds are self-clearing (`workflow_limit` or `step_limit`) and sort last in their workflow group in `workflow list`. `loop list` puts them in HELD. Reasons start with `workflow <name>: ` or `step <name>: `, followed by one of:

- `N of M agents open (@a, @b, +K)`: at most two handles, then `+K` for the rest, the suffix omitted when empty.
- `next start in <duration>`.
- `N earlier starts waiting` (singular `1 earlier start waiting`).
- `N starts launching` (singular `1 start launching`), while a launch of the same workflow is not yet counted.

Machine cap reasons name reservations separately (`1 more starting`) and a multi-agent layout's demand (`this start opens 2`).

### Failures

A node fails with one of eight causes, and each kind raises only its own:

| Kind | Causes |
| --- | --- |
| `agent` | `died`, `timeout`, `visits`, `capacity`, `deadline` |
| `command` | `died`, `timeout`, `visits`, `deadline` |
| `message` | `undelivered`, `visits`, `deadline` |
| `runs` | `died`, `timeout`, `visits`, `deadline` |
| `test` | `died`, `timeout`, `deadline` |
| `script` | `died`, `invalid`, `timeout`, `deadline` |
| `judge` | `died`, `invalid`, `timeout`, `errored`, `deadline` |
| `condition` | `timeout`, `deadline` |
| `sleep` | `deadline` |

| Cause | Raised when |
| --- | --- |
| `died` | The node's process could not start or ended abnormally: an agent launch that failed, including a failed continued restart; a turn step's provider exit before its clean turn end; a session's exit away from rest ([how an attempt ends](#how-an-attempt-ends)); an agent attempt a room rebirth ended; a command's or script's nonzero exit or signal; a test killed by a signal; a command, test, or script that could not spawn or open its log; a command whose host died; a judge's session that ended without an answer; a runs step that refused to start its runs, or one of whose started runs did not end `done`. |
| `invalid` | A script gave an answer the node cannot use (stdout that is not one JSON object, a wrong field type, an outcome its `on` does not name or a reserved word, or a bad `recheck_at`), or a definition edit left a judge's accepted answer unroutable. |
| `timeout` | The node's `timeout` passed. |
| `errored` | A judge's own turn ended on a provider error that waiting will not clear. A step's errored turn is not a failure: it takes the step's `on.errored`, or holds the step. |
| `undelivered` | A message step found no single living agent, its prompt could not be composed, or the address resolved to another session than its pinned `session`. |
| `visits` | An entry would exceed the step's `max-visits` plus its visit grant. Nothing launches and no visit is counted. |
| `capacity` | An agent step's capacity wait gave up, or its start can never fit under a machine cap. No visit is counted. |
| `deadline` | The run's deadline passed while the run was at the node. A live attempt or evaluation stops first. |

A failure takes the node's `on.<cause>` when set, else the node's `on.failed`, else `[workflow] on.failed`, else the run ends `failed`. A failure never follows `then` or an answer route, and a failure route may not be `@wait` (`a failure route cannot wait; route it to a node, @done, or @failed`).

- Validation refuses a `visits` or `capacity` route that, after this chain, can reach its node again, since those failures count no visit: `<path>: <workflow>: the visits route out of <node> leads back to <node>; it counts no visit, so route it where the run cannot return`, at the route that applies (`<kind>.<node>.on.<cause>`, `<kind>.<node>.on.failed`, or `workflow.on.failed`). So a catch-all `on.failed` that leads back through a check to a capped step refuses.
- A node that `[workflow] on.failed` names sets its own `on.failed` (to `@failed`, say). Its failures would otherwise fall back to the workflow's `on.failed`, which is the node itself, and a step there refuses as a cycle with no `max-visits` bound.
- The deadline fails the node the run is at once per grant. The node it routes to runs without a deadline until `extend --deadline <WHEN>` grants another.
- A fresh agent launch whose takeover could not stop the checkout's occupants, before it started anything, is tried once more: the step waits as [`retry`](#the-waiting-line) with no attempt or visit counted, the run's next advance launches again, and only a second failure is `died`. Each new entry into the step gets its retry back.

A condition's reading error and a sleep's evaluation error are not failures of the node: the run ends `failed` with the error as its reason and takes no route. A blocking question takes a step's `on.asked` when set and otherwise stays open in the agent's pane: it is not a failure. A judge's question stays open in its pane until its timeout. A `workflow cancel` and a `workflow restart` take no failure route. A definition edited under a run can also end it with no route ([live definitions](#live-definitions)).

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
| `errored` | `errored`, `{text, class, handle}` of the judge's errored turn: the error's text, its class (`failed`, `unknown`, or `not_entitled`), and the judge's handle |
| `timeout` on a judge stopped when it passed | `asked`, `{text, kind, handle}` of an open question; otherwise `stopped`, `{kind, handle, class, text}` of what it was stopped on, `kind` one of `limit`, `error`, or `budget`, and `reset_at` beside it when the reset of the agent `handle` names is known (a reset read from another agent is left out). Members not known are left out. |
| `capacity` | `waited_ms`, how long the start was held, and `first`, when the hold began; `never_fits: true` when the start is wider than a machine cap |
| `visits` | `visits`, the step's count, and `max`, its limit with the step's grant |
| `deadline` | `deadline`, the instant that passed |
| any other | none |

A judge's `errored` reason is the provider's error text, or `the judge's turn ended on a provider error` without one. A judge's `timeout` reason reads `<node> ran past its timeout <S>`, followed, when it was stopped, by what on: ` while <handle> asked a question: <text>` (`asked for permission`, `asked for plan approval`), ` while <handle> was stopped on a provider limit (<class>): <text> (resets <time>)` (`provider error` for an error, `the judge` without a handle, the class left out where it repeats the stop's noun, as a plain provider error's does, `(resets <time>)` only when it is that agent's own reset, the parts not known left out), or ` while <handle> was parked on its budget`. A question wins over any other stop, and the handle is the agent that asked, a subagent's included.

The attempt the failure route admits records the cause as its `routed_by`. `show --history` prints the route as `route <cause> → <target> · <fields>`, each of `reason`, `key`, and the `detail` members as `name=value`, joined by ` · `. A start wider than a machine cap is a configuration refusal: unrouted, it ends the run `failed`, and a loop task records a `failed` run with a strike, not `throttle skipped`.

## Check answers and the context file

### The check answer

A script prints exactly one JSON object, and a judge sends one through [`rimz workflow submit`](#workflow-submit):

| Field | Required | Meaning |
| --- | --- | --- |
| `outcome` | yes | The edge to follow, from the check's `on` table. |
| `reason` | no | Text for the history and a push-back prompt. |
| `recheck_at` | no | An RFC 3339 time. On `@wait`, nothing runs the check again before it, unless a subscribed signal arrives. For a `for` hold it is when the hold completes. |
| `key` | no | A string or integer naming the item or failure the run now works on, such as an issue number or `"lint"`. Later context files carry it. An answer without `key` keeps the current key. |

Any other field travels too: the whole object becomes the next node's `input`. `null` is not a valid value for `reason`, `recheck_at`, or `key`, so an answer cannot clear the key. A script must also exit 0: a nonzero exit fails it with `died`, and an answer it cannot use fails it with `invalid`.

Reserved outcome words: the eight causes (`died`, `invalid`, `timeout`, `errored`, `undelivered`, `visits`, `capacity`, `deadline`), `limited`, `failed`, `asked`, `unknown`, `hold`, and `trust`. A case or `else` that names one refuses, a script that answers one fails with `invalid`, and `submit` refuses one and says why. In an `on` table they are the engine's routes. A check's route refuses when the check cannot answer it (`dead route: this check cannot answer that outcome`): `on.hold` is always a dead route, `on.asked` on any check is one, and `on.errored` is one on every check but a judge.

### Test answers

A test's answer is `{outcome, code, log}` plus an inherited `key` when present. Exit 0 is `pass` and another exit is `fail`. `code` is an integer. `log` is the absolute `work/<seq>.check.log` path, holding stdout and stderr together. Only `fail` has `reason` (`exited N`). Output tails are not journaled. A spawn or log-open error, or a signal that kills the command (reason `killed by signal`), raises `died`, never `fail`, and its failure's `detail` carries no `code`. It carries the `log` once the log file exists, so a log-open failure's `detail` is empty. A timeout raises `timeout` and a run deadline raises `deadline`, and both stop the command's process group. A `fail` routed to `@wait` shows `fail` in the waiting line.

### Script answers

A script keeps stdout as its JSON answer and writes complete stderr, including an empty stream, to `work/<seq>.check.log` once it settles. Invalid answers journal the stderr tail.

### `workflow submit`

`rimz workflow submit <OUTCOME> [--reason <TEXT>] [--key <KEY>] [--recheck-at <WHEN>] [--json <OBJECT>]` is how a judge answers its check, from its own pane. The flags set the answer's fields: `--key` as text, and `--recheck-at` in any [workflow time](#workflow-times) spelling, resolved in the `timezone` setting and stored as an RFC 3339 instant. `--json` takes one JSON object whose other fields travel with the answer; a flag wins over the same field there. It prints `accepted <outcome> for workflow run <id>; this pane closes once the run settles it`. The run settles the answer at its next advance, closes the judge's pane, and routes.

`submit` refuses with exit 1 and its reason, leaving the run as it was, and the judge can submit again:

| Case | Reason |
| --- | --- |
| The caller is not an agent RimZ launched as a workflow judge, a judge's own child included | `caller is not a workflow judge; rimz workflow submit answers a check from its judge's own pane` |
| The caller is a step's agent | `caller is a workflow step's agent, not a judge; a step ends by finishing its turn` |
| The run ended, or moved past this judge's evaluation | `workflow run <id> is not active` or `workflow run <id> is no longer waiting on this judge` |
| The judge already answered | `workflow run <id> already has this judge's answer` |
| The judge's timeout has passed; the check fails with `timeout` at the next advance | `workflow run <id> has passed the timeout of the judge check <node>` |
| The outcome is reserved, not routed by the check's `on`, or a field has the wrong type | The [check answer](#the-check-answer) rule it breaks, then `; OUTCOME is one of` and the outcomes the check routes. |
| `--json` is not one object, or `--recheck-at` is not a workflow time | The flag's parse error, a usage error with exit 2. |

### Condition answers

A condition's answer has `outcome`, `readings` (a map from term key to string, or null when unread), and the winning zero-based `case` (omitted for `else` and `unknown`). The inherited run key is included when present: cases do not set keys. `unknown` includes a `reason` naming each unread key with its cause, as in `ci (no supported forge for this checkout), pr (no supported forge for this checkout)`. `--history` prints a cause that several keys share once. While a case's `for` hold runs, the outcome is `hold`.

The bare-`ci` commit fence sets `readings.ci` to null. When that makes the answer `unknown`, `reason` includes one of these, using the first seven characters of each commit OID (or all of a shorter OID):

- `ci (local HEAD <l7> is not on the PR, PR head <p7>)`
- `ci (read at <c7>, PR head <p7>)`

The local-HEAD reason takes precedence over the CI-commit reason, and the pre-entry-probe reason takes precedence over both. The same text follows `unknown: ` in the waiting line and JSON `detail`, and remains in the journaled answer `show --history` prints.

### The context file

RimZ rewrites `$RIMZ_WORKFLOW_DIR/context.json` before every check and every attempt. Scripts and command steps read it. The Workflow launch reminder's visit numbers match the attempt's `visit` and `visits`.

| Field | Meaning |
| --- | --- |
| `run`, `node`, `seq` | The run id, the current node, and this evaluation's sequence number. |
| `prompt` | The run's `--prompt`, or null. |
| `input` | The answer that routed here, the [failure input](#the-failure-input) after a failure route, or null at the start (the `run --input` object when given). |
| `key` | The current item key, or null. |
| `visit`, `visits` | Steps only: the step's visit count stamped when this launch or push-back was admitted, and its current count, both since its last manual restart. `max-visits` caps them. |
| `step_visits` | Every node: an object from each visited step's name to its visit count, as `visits` counts it. `{}` before any step is visited. |
| `key_visits` | Every node: an object from each step's name to a list of `{"key": <key>, "visits": <n>}`, one entry per item key the step was visited under, in the order the keys first came. `key` is as the run holds it, so `7` and `"7"` are two items. A visit with no key counts only in the step's total. `{}` when no visit had a key. |
| `signs_path` | For a step, its sign file, `signs/<step>.md`. For a check, the `signs/` directory. |
| `prev_attempt` | How the last step attempt ended, or null. |
| `live_attempt` | The open session of a step with a `rest-check`, during its rest checks too. Absent for a turn, a session without a `rest-check`, or a staged ending. |

`prev_attempt` remains the last attempt that ended: running a session's rest check does not replace it. Its fields:

| Field | Meaning |
| --- | --- |
| `outcome` | `ended`, `died`, `timeout`, `asked`, `errored`, or `canceled`. `asked` occurs only with `on.asked` set, and `errored` only with `on.errored` set. A command step ends `ended`, `died`, `timeout`, or `canceled`. |
| `routed_by` | The outcome that admitted the attempt, or the cause when a failure route did. Null for a start step, unless `run --input` gave it an outcome. |
| `key`, `visit` | The attempt's key, and the visit count stamped when its launch or push-back was admitted. |
| `started_at`, `ended_at` | The attempt's start and end times. |
| `handle` | The leader's handle. Null for a command step. |
| `head_start`, `head_end` | The commit before and after. A command's `head_end` is null when another process, rather than the host that reaped the child, settles the attempt. |
| `cost_usd` | The leader's known cost. Null for a command step. |
| `asked` | The question it asked, when it ended on `on.asked`. |
| `errored` | `{text, class, handle}` of the leader's errored turn, when it ended on `on.errored`: the error's text, its class (`failed`, `unknown`, or `not_entitled`), and the leader's handle. |
| `code`, `log` | A command step's exit code (null after a signal or an unobserved exit) and its output file when one exists. Null for agent attempts. |

`live_attempt` is `{step, seq, visit, visits, key, started_at, handle, run_id, claim_path, head_start, turn_end}`: `turn_end` is the last consumed turn identity, or null. It carries no outcome or end evidence. During a check, the top-level fields describe that check, without `visit` or `visits`, while `live_attempt` describes the session. A push-back or discard rewrites the file to the step view, a held `@wait` keeps the check view, and a close leaves it until the next publication.

## Validate and schema

`rimz workflow validate [DIR] [--json]` checks one directory, or every directory under `~/.rimz/workflows/` whose name does not start with `.`. A single normal path component names a workflow under that home, as in `run`. Any other path is checked where it is, inside that home or not, and its name must still be a valid workflow name. A name may instead select a loop task's one-check graph, as in `show`, with the same `workflow:` and `task:` selectors ([loop task graphs](#loop-task-graphs)). Profiles and teams resolve from the machine's definitions either way. Condition and sleep stage names use the caller's effective teams plus `Done`. Without a resolved workspace, a warning says stage names were not checked. Starting a run always checks against the run's project.

The table has these columns:

| Column | Holds |
| --- | --- |
| `WORKFLOW`, `NODE`, `KIND` | The workflow, the node, and the table word it is written under. |
| `RUNNER` | The run command, shell text, step layout, judge agent, `message`, `condition`, `sleep`, or `workflow` for a runs step. JSON's `message` field carries a message step's address. |
| `BACKGROUND` | For agent steps and judges, whether the agent reports background work (`yes` or `no`). `—` for every other node. |
| `EDGES` | Each edge as `outcome → target (source)`, or `outcome → target (source, mark)` when conditional or unable to fire. |

An edge's `source` is `explicit`, `on.failed` (the node's catch-all), `workflow.on.failed`, or `default`. A step's `asked` edge appears only when `on.asked` is set, always as explicit. A session's rest checks appear as explicit `rest-check → <check>` edges.

| Mark | Meaning |
| --- | --- |
| `never: no timeout` | The agent step sets no `timeout`. |
| `never: no max-visits` | The step sets no `max-visits`. |
| `only with a deadline` | The definition sets no `[workflow].deadline`: the edge fires on a run started with `run --deadline` or granted one by `extend --deadline`. |

Rows follow the graph from `start`, with a returning step immediately below the earlier check that sends it. Unreachable nodes come last in name order. A check's edges follow case order without repeats, then `else`, then other explicit outcomes in name order. A step's edges start with `then`, then `asked`, then `errored`. One edge per cause the node's kind raises follows, in the order of the [cause table](#failures), each with the target its chain resolves to. A session's `rest-check` edges come last, in list order. Command rows list `then`, `died`, `timeout`, `visits`, and `deadline`. Message rows list `then`, `undelivered`, `visits`, and `deadline`. An agent step's `capacity` edge always fires: a start wider than a machine cap raises `capacity` on any run, a `--skip-capacity` run included. A sleep's edges are explicit `then → <then>`, default `sleeping → @wait`, default `unknown → @wait` only with `when`, and `deadline`, in that order, with no timeout edge.

Below the table, each error prints as `<workflow>: <path>: <message>`, then each warning as `warning: <workflow>: <path>: <reason>`, all on stdout. Any error exits 1 and adds `error: N workflow error(s)` on stderr. Warnings alone exit 0. Marks explain edges: they add no warnings and relax no admission rule.

Warnings:

- When auto-continue is off, each agent step warns at `agent.<step>` that a step paused on a provider limit waits for you. A judge does not warn: its timeout runs through a limit.
- Turn steps and sessions with a `rest-check` warn when the leader adapter has no background reporting: a turn end may finish the attempt while provider background work remains.

### Refusals

Every table refuses a field it does not take, including a field another kind owns, at its dotted path with zero-based array indices, such as `agent.fix.takeover` or `condition.pr.cases.1.for`: `` agent.fix.takeover: <workflow>: agent.fix.takeover: unknown field `takeover`, expected one of `summary`, `agent`, … ``. A wrong-typed value refuses the same way, and so does a name two kinds share (`test.fix: <workflow>: test.fix: agent.fix has the same name; a node name is unique across every kind`). These are parse errors, so the definition reports the first one alone, as `<path>: <workflow>: <error>`.

An invalid value in an optional field refuses at its path as a required field's does. Nothing a definition writes is read as absent. That covers:

- a blank or multiline `summary`, an unsupported `deadline`;
- a `timeout`, `stagger`, `for`, or `sleep` that is not an `s`/`m`/`h`/`d` duration within `365d`, and a zero `stagger`, `for`, or `sleep`;
- `give-up` that is zero, an unsupported `window`, `max-visits` or `max-parallel` below 1;
- an invalid `wake` or `subscribe` entry ([wake and subscribe](#wake-and-subscribe));
- an unknown condition or sleep `provider`, a `provider` with no window term, an `account` without `provider`;
- an invalid agent `account`, `mode = "plan"`, a `budget` outside the `--budget` grammar, an invalid `worktree` name (empty is allowed), and a blank message `session`.

Route rules read the definition as written:

- A check's route refuses when the check cannot answer it (`dead route: this check cannot answer that outcome`): a reserved word other than `failed` and the causes the check's kind raises (which a condition extends with `unknown`), or a condition outcome no case or `else` declares.
- A target that names no node, a failure route to `@wait`, and a work node's edge to `@wait` refuse.
- A check-only cycle needs `@wait`, and a cycle through work nodes needs `max-visits` on one of them. Every route except a `deadline` route counts as an edge.
- A node's `visits` or `capacity` route, resolved through the [chain](#failures), refuses when it can lead back to that node. The walk does not follow a `deadline` route, which fires once per grant.

### JSON

`--json` prints `{rows, warnings, errors}`, each error and warning carrying `path` and `message`. Each row has `workflow`, `node`, `kind`, `runner` (`"run"`, `"shell"`, `"agent"`, `"message"`, `"condition"`, `"sleep"`, or `"workflow"`), nullable `summary`, `run`, `shell`, `agent`, and `message`, `do_background`, and resolved `edges`. Each edge keeps `outcome`, `target`, and `source`, and adds `fires`: `"yes"`, `"never"`, or `"with-deadline"`. Only `"never"` edges carry `because`: `"no-timeout"` or `"no-max-visits"`. Command and message rows have `agent = null` and `do_background = false`. The row and edge orders above apply.

### Schema

`rimz workflow schema` prints the JSON Schema of `workflow.toml`, derived from the loader's own tables, so an editor flags an unknown field (every table keeps `additionalProperties: false`), a value of the wrong type, and a field the loader always requires, such as an agent's `then`. Rules that depend on other fields or on the node's routes, such as `run` or `shell`, a judge's `summary`, and a test's `on.pass` and `on.fail`, are checked by `validate` alone.

## Inspect definitions

`rimz workflow show <WORKFLOW>` draws the current definition in walk order, with returning steps hung beneath their earlier sending check.

- Checks draw `◇`, steps `○`. `→` points to a later node and `↑` to an earlier node or itself. Unreached nodes follow under `unreachable from <start>`.
- The header names the summary, scope, any workflow `limit`, and any configured deadline route. For a deployed workflow it names each loop task added with `--workflow <WORKFLOW>` and its trigger (`started by loop task <task>: <trigger>`). With none it reads `no loop task starts it`, even when a shell task's command runs `rimz workflow run` on it, and gives a `rimz loop add --workflow` command and the by-hand `rimz workflow run` command. A loop task's graph names its own task.
- Each node shows its runner, summary when set, routes, and routing limits: timeouts, visit caps, windows, `takes over` or `shares the checkout`, bounded `give-up` waits, and the lifecycle (`1 turn` or `session`). A message step's runner is its target address.
- Condition cases keep their order and hold durations, followed by `none true`. A note names where an unread term goes.
- Edges that cannot fire are omitted, and so is each failure edge left to its default `@failed`, a step's `capacity` edge included. An explicit `on.capacity` draws it.
- The footer states the uniform turn-end, invalid-check, and unset-deadline defaults once, only when used, then names the source file and a command for one node in full.
- The live-runs table follows the definition.

A missing or unloadable definition prints `<workflow>: definition unavailable: <error>`, then still lists its runs and exits 0. An unloadable one adds `; check with rimz workflow validate <workflow>`; a missing `workflow.toml` has nothing to validate, so it adds no hint. An unknown workflow with neither a definition directory nor runs refuses. `show <WORKFLOW> --json` prints the live-runs array ([JSON shapes](#json-shapes)) and no rail.

### One node

`rimz workflow show <WORKFLOW> <NODE>` prints one definition node in full:

1. The runner kind and summary.
2. Predecessors with the outcomes that reach it. Never-firing routes are omitted, and conditional ones carry the shared mark.
3. Check subscriptions (derived for conditions, explicit otherwise) and the minute tick, and a step's `receives` subscriptions with their resolved scope (`for this checkout`, `for this checkout's team board`, `where k=v`, or `anywhere in the room`).
4. Ordered condition cases with holds.
5. Every resolved edge with its source and fireability mark, including never-firing edges and a default `capacity` edge. A sleep's view leaves out its default `sleeping → @wait` and `unknown → @wait` edges, which `validate` lists.
6. The configured fields: command text (in full, multiple lines included), message target, agent layout, prompt path, lifecycle, `rest-check` (joined by `, `), occupancy, keep, window, throttle, limit, give-up, visit bounds, timeout, provider, and account. A limit reads `max-parallel=N · stagger=<duration>`, omitting unset fields. The definition header prints its workflow limit in the same form.

The two-positional form takes a workflow name only. A run address or run id is a usage error (exit 2), except that a finished run's id or address refuses with exit 1, saying the run is finished and naming the `rimz workflow show <run>` that prints its saved history. It conflicts with `--json` and `--history`. An unknown node refuses with the definition's node names (exit 1). A missing or unloadable definition refuses with the same text (exit 1), with no runs-table fallback.

### Loop task graphs

`<WORKFLOW>` also names a loop task in the caller's project that runs a command (every task but a delivery row). `show` draws the one-check graph each fire runs, the footer reads `compiled from loop task <task>`, and the live-runs table lists that task's runs. A name that is both a deployed workflow and such a task refuses (exit 2) and names both selectors: `workflow:<name>` picks the deployed workflow and `task:<name>` the task, in `show`, `show <WORKFLOW> <NODE>`, and `validate`. `task:<name>` for a task that runs no command refuses with `no loop task <name> runs a command` (exit 1).

## Inspect runs

### `workflow list`

`rimz workflow list [--json] [--all [-n N]]` prints every unfinished run across rooms, without column headers. With none, it prints `no active or parked runs; start one with rimz workflow run <name>`, and `--json` prints `[]`; under `--all`, the FINISHED section follows that line and `--json` holds the finished rows.

NEEDS YOU comes first: parked runs, active runs with a blocking ask, active runs held on an error that waiting will not clear (a provider error, an unknown error, a lapsed plan, or a failed turn with no error on the card), capacity holds needing a fix (`check_failed`, `disk`, `store`), and runs waiting `trust`, longest waiting first, with full addresses and a `workflow show` hint for the first run. The other runs group by workflow name, each heading reading `<workflow> · N active[ · M held]`. Self-clearing capacity holds sort last in their group, and each part sorts by address. Several rooms get headings (the home-relative project root), the caller's room first, and hints for other rooms carry `--root`.

| Row | Form |
| --- | --- |
| NEEDS YOU | `<glyph> <address>  <detail>  <age>` |
| Grouped | `<glyph> #<checkout>  <detail>  <age>` |

| Glyph | Meaning |
| --- | --- |
| `?` | An agent's blocking ask. |
| `⊘` | A capacity hold needing a fix: the `workflow.held` glyph, also in the `show` headline and current rail node. It is not an agent question or a paused agent. |
| `◉` | A self-clearing capacity hold. |
| `⢿` | A with-you session or active work. |
| `☾` | Another active wait. |

Glyphs come from the configured glyph set ([theme roles](../../guide/theme.md#glyphs)). A group names `started by <origin>` when all its runs share their origin. Otherwise each row adds ` · by <origin>`. An origin is `hand`, `loop task <task>[, fired by hand]`, the starting agent's handle (`agent launch <launch>` once the agent has left the room), or `run <run> at <node>` for a run another run started. A group names one origin only when its runs share the loop task, the manual-fire flag, and the starter. Age starts at the park, at the opening of an ask, at the start of a step's hold, at the first capacity hold, or at node entry otherwise. Detail truncates before age at narrow terminal widths. Each row whose definition loads is followed by the horizontal rail ([the rail](#the-horizontal-rail)).

Detail is one of:

- `asking you: <handle> (<kind>): <one-line text>` for a blocking ask.
- `with you in @<handle>` for a live session, with ` · checking <check>` while held at a rest check and `: <wait phrase>` at that check, or ` · <check>: <wait phrase>` after it when waiting. The handle matches the agent card, without the channel the row already names, and falls back to the recorded pet name.
- `waits on its definition: <first error>` for a run whose definition cannot be read, does not parse, or is refused by validation ([live definitions](#live-definitions)). It comes ahead of a capacity hold. JSON keeps its usual `detail`.
- `<handle> held <age>: <stop>` for a step held on a stop ([above](#how-an-attempt-ends)), with a session's check and wait suffix.
- `held · <reason>` for a capacity hold.
- Otherwise the [wait phrase](#the-waiting-line), `running` (with ` · visit <n> of <max>` when capped), or `checking`.

A try's detail opens with `once · `.

A cached PR number adds ` · PR #N` once per row. Each room supplies its own agent names and PR cache. JSON reads neither that enrichment nor the journal: its `detail` reads `with you` without an agent or PR, and `held <age>[, gives up <clock>] · <reason>` for capacity waits, without node or the separate age column.

`--all` adds a `FINISHED` section after the live list: the ended runs of workflows started by name, in the same rooms, newest ending first. It reads each run's record header, so a run an older RimZ wrote is listed too. A workflow task's fires are listed; the run a shell task's fire wraps its command in and a run whose record cannot be read are left out. A run that ended but has not yet moved to `finished-workflows/` is listed here as well, and a run the live list shows is never also a finished row. Several rooms get headings inside the section as in the live list, the caller's room first. A row reads `<glyph> <address>  <state>  <id>  <ended>`, with `once · ` before the state of a try that did not end `tried`. Within a room, adjacent rows that agree on everything but the id and the ending time fold into one, which names the newest id and reads `<state> ×N over <span>`. `-n` (default 20) limits the rows printed, and a closing line names the runs it hid and the `-n` that shows them. With no ended run the section is left out, and `-n` without `--all` is a usage error. Finished runs are kept about seven days after their last write ([finished runs](#finished-runs)).

### `workflow show`

`rimz workflow show [RUN] [--history | --json]` defaults to the calling agent's workflow run, including from its subagents. Outside a workflow, an omitted RUN refuses with exit 1 and points at `rimz workflow show <run>` and `rimz workflow list`. The node view still requires an explicit workflow name. When a live run [waits on its definition](#live-definitions), the human view and `--history` print the run from its record alone and exit 0: a headline saying `its definition cannot be read`, `its definition does not parse`, or `its definition is refused`, then `waits on its definition: <first error>`, `check with rimz workflow validate <folder>`, and `or end it: <cancel command>`. `--json` prints the record and journal either way.

The compact view of an unfinished run prints, in order:

1. `<address>[ · once][  PR #N]`. ` · once` marks a live try. The PR badge shows a cached PR number for active or parked runs.
2. The activity glyph and the state headline. Only the state phrase takes the state tone. Agent context uses body tone, and the bound forecast is muted.
3. On an active run, one saved readings line from the current check's latest evaluation since entry: `key=value` pairs separated by two spaces, null as `key=unread`, then ` · read <clock>`.
4. The `now` line, for a condition check or a sleep with `when` ([below](#the-now-line)).
5. The origin line: when and how the run started (its [origin](#workflow-list): by hand, by loop task, by an agent, or by another run), the number of attempts started (`1 attempt` while the first one runs, `0 attempts` for a run that has started none), and the known total cost of the ended ones, including judge evaluations. It omits cost when none is recorded, and never estimates a live attempt's spend.
6. The block below the origin: the full ask text when asking, `takeover:` lines naming the agents that hold the checkout (`blockers unavailable` when the roster or the project's worktrees cannot be read), and for a parked run why and the `you can` commands (resume, grant more bounds, choose another node, or cancel).
7. The definition rail ([below](#the-definition-rail)) on an active run, or the horizontal rail after `you can` on a parked run.
8. `visits  <step> <used>[ of <max>][ · key <key>: <n>]` for each visited step that is capped or has visits under the run's current key: ` of <max>` for a capped step, its grant included, and ` · key <key>: <n>` for the visits charged under the current key. Counts run since the step's last manual restart. An uncapped step with no visit under the current key prints no row. After the visits rows, a run at a [runs step](#runs-steps), or one that has joined its started runs, prints one row per started run: `started <id> <workflow> <state>[ · <checkout>]`, where the state is the started run's own (`unreadable` when its record cannot be read, `ended` once its home is gone, `not started` before it exists).
9. The `trail`: routing decisions projected from the definition and journal, consecutive matching check answers folded as `×N`, with resumes, restarts, parks, and termination. It keeps the newest eight hops with `+N earlier` for the rest, and ends in `now` while live.
10. The home-relative work directory, then `sign <path>` in the same style while the run is at an agent, message or runs step whose definition loads: the step's sign file ([prompts](#agent-steps)), printed whether or not it exists, and left out on an `asked` park, whose `you can` lines name it. Then the absolute notes folder when the run has one, and the command for full history.

The headline:

| State | Headline |
| --- | --- |
| Running step | The step, its visit limit, and the agent's full card handle (channel included, recorded pet name as fallback). |
| Running check | `checking <check> <age>`, with ` in <handle>` while a judge's agent is up. |
| Session | `<step> is with you`, plus the same check and wait suffix as `list`, not repeated below. |
| Held | `<step> is held <age> · <handle>: <stop>` for a step of either lifecycle, or the current judge's check, held on an error, a limit, an overload, or a budget park. A held judge's line ends ` · the check timeout keeps running`. The headline takes the waiting tone when the hold needs you. |
| Asking | `<step> is asking you · <handle> (<kind>)`, with an instruction to answer in the pane or with `rimz answer`: the step's line ends `the step timeout is paused`, a judge's `the check timeout keeps running`, and either reads `no <kind> timeout is set` without one. A judge's question names its check. |
| Waiting | The wait phrase with its node, elapsed time, recheck, and the next bound's time and target. A hold names its outcome, duration, and the next bound. |
| Parked | `parked at <node> <age> · <reason>`, with the explanation in the block below. |
| Ended, before its directory moves | `done`, `failed`, `canceled`, or `tried`, with no PR badge. The headline ages from the ending. Once the directory is in `finished-workflows/`, `show` prints the [finished view](#finished-runs). |

The forecast picks the earliest of the unfired deadline, a held session's timeout, the check or step timeout, and a hold's completion, in that order on ties. A held session's timeout names where the step's `timeout` failure goes. A timeout block shows the flushed wait as `<node> said <outcome>` with a folded count and span, and the last shell-check log. `invalid` shows the recorded stderr tail.

The `you can` commands, history footers, receipt hints, and NEEDS YOU hints name the run by address when it selects this run, otherwise by id, and add `--root` for another room.

When a step of a `--skip-capacity` run launches past a hold, `show` prints `<holds>; started anyway: the run skips capacity` for its current session, including while held at a rest check, and `--history` prints it inside that attempt's block after settlement. The holds are sampled when the step claims admission (for restarts and later steps too), not at `workflow run`, which prints no such line. They may name an in-flight launch, an earlier waiter, a full cap, a stagger gap, a machine hold, or a capacity check that declined or failed, joined by `; `. A start that met no hold adds no note. JSON carries the text in optional `started_past_limit` on the live attempt and the journal attempt.

#### The `now` line

A human view of a live or parked run shows one `now <check> · <outcome>` line for its condition check or its sleep with `when`. At an agent step it reads the first rest check, else the `then` check. At a parked check it reads that check. It shows what that check would answer from current cached readings and the board, not its last journal answer: current `key=value` readings, null as `key=unread`, and the evaluator's unread reasons. A winning case with `for` shows the remaining hold from its open wait, or the full duration without one. Forge keys add ` · read <clock>` for the cached probe, and board-only checks omit it. The freshness floor is the step or parked node's entry, or the held session's entry while held. The line launches no check, writes nothing, and refreshes no probe. The CI fence may make read-only Git calls. Script, shell, judge, and plain-sleep checks have no `now` line. JSON and history do not carry it.

#### The definition rail

Active runs paint the definition rail after the block below the origin and before `visits`, from the run's definition as it is now, so an edit to a deployed folder shows on the next `show`.

- Reached steps draw `●`, reached checks `◆`, with green glyphs and body-tone names. Unreached steps draw `○`, checks `◇`, in muted tone.
- Current nodes use the headline's glyph, bold in the state tone, including both the session step and its check while held at a rest check.
- Reached nodes add their last check outcome or completed attempt (outcome, duration, known cost, recorded HEAD before and after), its local clock, and `×N` for repeat entries. A capped step adds `visit <used> of <max>`, including the step's grant. Equal recorded commits read `HEAD <a>, no new commits`, different ones `HEAD <a> → <b>`. Completed attempts omit pet names.
- The current check names its outcome, `since <clock>`, `last checked <clock>` from its latest evaluation, its wake selectors, and the minute tick, with no advance count. A current session adds `receives` and its subscribed selectors.
- Condition cases: false cases are faint. The winning or held case reads `│ ▸ N`, bold blue, or green for `@done` and alarm for `@failed`. The first unread case keeps `?` and `<key> unread: <reason>`. Later cases are unmarked and muted, except that `@done` and `@failed` cases keep their definition tone. A held case shows its elapsed and required duration and target, with a completion clock only when that completion is the next bound.
- Marks stay visible with `NO_COLOR=1`.

#### The horizontal rail

Parked runs and `list` rows use a horizontal rail.

- Steps draw `●` when reached and `○` otherwise. Checks draw `◆` and `◇`. The current node uses the headline's activity glyph. Reached glyphs are green with body-tone names, current nodes bold in the state tone, unreached nodes muted.
- A finished run has no current node. Its ending node is where it stopped: alarm-colored for failed runs, green for done or canceled runs. With colour off, the ending node draws as any other reached node.
- Nodes join with ` ─ ` in walk order. A visited, current, or ending detour hangs below its sending check as `╰─ <glyph> <step>[ ×N] ↑`, with the visit total above one.
- Unreachable nodes appear after the main rail only when current or reached, including after `resume --at`.

To fit a terminal, the rail drops nodes from the left as `… `, then from the right, reached or not, always keeping the current or ending node. Next, nodes between a held run's two current nodes drop as ` ─ … ─ `, then the predecessor, which stays only while it fits. If that still does not fit, visited detours under the kept nodes drop, except those that are current or ending, and dropped nodes return wherever they fit. A detour drops with its parent unless it is current or the ending, which keeps its parent visible. Names are never shortened, so a current or ending node wider than the terminal is drawn whole. Piped rails are not shortened.

### History

`--history` replaces the compact view with the address, state headline, and the journal in recorded order, with local clocks and readable facts. With an explicit target it requires an address or id, not a workflow name, and it conflicts with `--json` (usage error, exit 2).

| Row | Prints |
| --- | --- |
| Check answer | `<node> said <outcome>`, then `→ <target>` only when it routed, with readings, reasons, shell logs, judge cost, and invalid stderr tails. A route that repeats the preceding answer takes no row. |
| Abandoned check | `<node> said nothing · <outcome>`. |
| Attempt | Visits, the agent (its card handle while it is still in the room, as the headline names it, and its recorded handle once it has left) or command exit, duration, cost, short HEAD before and after, logs, reasons, stopped and live children, questions, and prompt and script digests (first 12 hex characters). A command step adds `    exit <code\|—> HEAD <start8\|—> → <end8\|—>`, then `    log: <path>` when present. Any attempt with a reason adds `    reason: <text>`. A message step's attempt names the agent it reached, or `—` and why no agent took it. |
| Failure route | `route <cause> → <target> · <fields>` ([failure input](#the-failure-input)). |
| Other route | Its target and input fields. |
| Wait | Outcome, count, and span. |
| Turn end | The action, check, outcome, and target (the target only for `kept`, `pushed`, and `closed`). |
| Park | `park <reason>`. The text is omitted when the newest printed answer or route row at the same node already printed it as its `reason`. |
| Restart | `<step> restart · fresh\|continued · visits restarted at 1`. |
| Cancel | `cancel canceled`, or `cancel canceled · <reason>` when the row has one. |
| Start | `start by hand` for a run started with `--at` or `--input`, with ` · input <object>` when given. |
| Terminal of a try | `terminal tried · stopped after one node; the run would go → <target>`. |
| Signal | `signal <name> → <handle>`, naming the leader it went to as an attempt row names its agent. |

The card handles are display only: the history text a run's ending saves into its home and notes keeps the recorded handles.

Resumes, terminal states, and wake signals print their recorded facts. Empty stopped and live child groups are omitted.

Folding:

- Adjacent check answers with the same node, origin, outcome, and condition case index fold into one row, whatever their reasons or other fields. The row uses the newest time, `#first–last` (or `#—` without sequences), `×N over <span>`, the newest answer, and the summed judge cost. The span runs from the oldest to the newest member, and `over <span>` is omitted under one second.
- Adjacent wakes fold when their node and signal agree. A wake between answers keeps its own row and breaks their run.
- A closing check-wait summary takes no row when an earlier answer row at its node with its outcome overlaps the wait's inclusive span. Other waits print `×N over <span>` when N > 1, then ` · <detail>` when present, except an `occupied` wait, which names the occupants its detail holds in its phrase (`occupied by <handles>`) and adds no detail after the count. Omitting a summary does not break an answer run.
- `--json` keeps every journal entry unfolded, including closing waits and duplicate routes.

Live runs keep the compact view's headline and clocks. A finished run's history uses its recorded end in the headline and date-qualified local clocks (`YYYY-MM-DD HH:MM`) on every row, so a later view or a GC backfill reproduces the saved text.

### The waiting line

The waiting line is the phrase a run at a wait carries, without the node. A single-run `show` puts it in its state headline, after the check a `with you` headline names. The same phrase is the `detail` string in `workflow list --json`, and `workflow_runs[].detail` in `loop list --json` and `loop show --json`. Status phrases print neither `said` nor an advance count, since an advance can find the run waiting without evaluating its check. History and park answer rows keep `<node> said <outcome>` and their fold counts.

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
| `sleeping <elapsed>/<sleep>[ when <terms>] → <then>` | A sleep's entry-based clock. It replaces every other phrase at a sleep, unread and held conditions included, and carries no recheck clock. |
| `unknown: <keys and reasons>` | A condition cannot read enough data to choose its first matching case ([reason forms](#condition-answers)). |
| `window <term>` | An agent step or judge whose provider `window` is closed, with the failing term and its reading, such as `window 5h.left=12<40`. |
| `budget <reason>` | An agent step or judge whose launch a room or account dollar budget would refuse, with the gate's reason, such as `budget fleet budget exhausted ($50.21 of $50.00/day)` and the command that raises it. The first advance after the budget allows the launch launches it. |
| `takeover` | The checkout holds an agent mid-turn, so the step cannot stop it yet. A step with `occupancy = "share"` never waits here. |
| `occupied by <handles>` | The checkout holds an idle agent and the step's `occupancy` is `wait`, the default. The handles are the occupants the last read found, each with its channel, such as `occupied by @coder#probe, @reviewer#probe`; a wait recorded without them reads `occupied`. |
| `needs trust: <problems>` | An agent step or judge whose launch was refused on reporting hooks or folder trust the user can fix in place. The detail holds each problem with its fix. The run needs you; the first advance after the fix launches it, and only the run deadline ends the wait, or, for a rest-check judge, its held session's timeout. |
| `retry: <reason>` | The step's launch failed stopping the checkout's occupants, before it started anything, and the next advance (the scheduler's minute tick or a hook wake) launches it once more. A second failure is a counted `died` attempt. |
| `held <age>[, gives up <clock>]` | An agent step held by machine capacity, a workflow or step limit, or a start queue still busy after 30 seconds. The give-up time is absent for `forever` and when nothing bounds the wait. The reason follows ` · `. |
| `recheck` | The check's `recheck_at`, before which nothing evaluates. A signal matching the check's subscription clears it. For a `for` hold it is when the hold completes. |

Every instant, capacity bounds included, uses local `HH:MM` today, `Mon HH:MM` within six days in either direction, and `YYYY-MM-DD HH:MM` beyond that.

## Run signals

Every run fires signals in the reserved `workflow` [family](./events.md#reserved-families) as it moves, with source `lifecycle`. Each committed write of the run record fires each signal its change calls for once, after the write:

| Signal | When |
| --- | --- |
| `workflow.moved` | The run's node changed and the run did not end. A write that passes through several nodes fires one, from the node the record held before it. |
| `workflow.waiting` | The run starts to need a person: the runs `workflow list` puts under NEEDS YOU. |
| `workflow.ended` | The run ended `done`, `failed`, or `canceled`. |
| `workflow.failed` | Beside `workflow.ended`, when the run ended `failed`. |

The room's workflow pass fires `workflow.ended` and `workflow.failed` for a run an older RimZ left that it [ends as failed](#runs-this-rimz-cannot-continue). The run a [shell loop task](#loop-tasks)'s fire makes for its command fires none, so a task on `workflow.ended` never fires itself again. A `--workflow` task's run fires them as any run does, so such a task subscribed to its own workflow's `workflow.ended` starts a run again at each ending. Every payload carries `run` (the run id), `workflow`, `node` (the node after the write), and `checkout` (the run's checkout path). `workflow.moved` adds `from`, and `workflow.ended` and `workflow.failed` add `outcome`. A signal is not replayed: a process that dies between the write and the fire loses it. The trigger line names the run ([subjects](./loop.md#the-triggers-evidence)).

A loop task, a check's `wake`, and a session's `subscribe` hear these signals as any other room-wide family. A run's own `workflow.*` signals never wake its checks or reach its leader.

## Wait for runs

`rimz workflow wait <RUN>... [--any] [--timeout <DURATION>] [--json]` blocks until the named runs end. Each `RUN` is a run id or a `<workflow>#<checkout>` address. A run id found in no home refuses at once with `unknown workflow run <id>` (exit 2), and so does a bare workflow name. The wait reads run records every half second, changes nothing durable, and holds nothing open.

Without `--any` it returns once every run has ended, and with `--any` once the first has: among runs one look finds ended, the one with the earliest end stamp, and an ended run ranks before a run the same look finds needing a person. A run whose home is deleted under the wait reads as ended `done`, since only a quiet loop task run's home is deleted, and only once it ended `done`. It also returns as soon as any run needs a person, with or without `--any`: a parked run, an agent of the run asking, a step held on an error waiting will not clear, a capacity hold that needs a fix, or a `trust` wait (the runs `workflow list` puts under NEEDS YOU). From an agent, it returns when a prompt for that agent is queued, printing the stop line `rimz teams wait` prints. `--timeout` bounds the wait, and the runs still going are left running.

It prints one line per awaited run (only the ended one under `--any`):

| Run | Line |
| --- | --- |
| Ended | `<address> (<id>): <state> · checkout <path> · notes <path>` |
| Needs a person | `<address> (<id>): needs you: <detail>` |
| Still going | `<address> (<id>): still <state> at <node>` |

`--json` prints an array of `{id, address, outcome, exit}`, `outcome` one of `ended`, `needs_you`, or `running`. An ended run adds `state`, `ending`, `checkout`, and `notes`, a run that needs a person `reason`, and a running one `state` and `node`; a running run's `exit` is the exit the wait stops with (`3`, `75`, or `124`).

| Exit | When |
| --- | --- |
| `0` | Every awaited run ended `done` (with `--any`, the first to end did). |
| `1` | An awaited run ended `failed` or `canceled`, or another failure. |
| `2` | An unknown run id or a bare workflow name. |
| `3` | An awaited run needs a person. |
| `75` | A prompt for the calling agent is queued. |
| `124` | `--timeout` passed. |

## Cancel and resume

### `restart`

`rimz workflow restart <RUN> [--continue]` replaces the current agent step, including a session held at its rest check or a run parked at an agent step. It stops the owned layout, keeps input and key, resets that step's visit count, and starts at visit 1. Other steps' counts and every visit grant do not change. Its relaunch waits for the step's usage window, checkout occupancy, and capacity as any start does, and a layout wider than a machine cap raises `capacity`. The step's `timeout` counts from the relaunch. Restart cancels any unconsumed push-back from the abandoned attempt, including one already sent to the pane, so it cannot delay judgment of the replacement's next clean turn. `rimz agents restart @<step-agent>` routes through the same continued restart.

Without `--continue`, the replacement starts fresh. With it, the leader resumes the current attempt's recorded conversation under the room's current account. From a parked agent step, it uses the previous attempt only when that attempt belongs to the same step. A still-live previous card is stopped before relaunch, and companions start fresh. If resume preparation fails after the stop, the step fails as `died`.

These refuse before anything stops:

- A definition that does not read, or that validation refuses, with the error `workflow advance` reports for it. `rimz agents restart` on the step's agent refuses the same way.
- A missing conversation, an unsupported provider resume, or a different account's history pool, naming a fresh `workflow restart` or `rimz accounts use` fix.
- A finished run: `workflow run <id> has ended (<state>); cannot restart` once its home has moved to `finished-workflows/`, and `workflow run <id> is <state>; cannot restart` in the short window before the move, where the state is `done`, `failed`, `canceled`, or `tried` for a try.
- A run being canceled, `workflow run <id> is canceling; cannot restart`, and a check without a held session.
- A command or message step: `node <node> is <kind>[; @user: <park> | ; waiting on <outcome>]; restart requires an agent step`.
- A restart already pending: `this step is already restarting`.

The command waits up to 30 seconds for the replacement to launch, park, or end the run, then prints one receipt line and a `workflow show` hint, followed by the held line `run` prints when the replacement waits in front of the step. The mode is `fresh` or `resumed (<session>)`.

| Outcome | Receipt |
| --- | --- |
| Launched | `restarted <address> step <step> as <handle> · <mode> · visits restarted at 1` |
| Parked | `restart <address> step <step> · <mode> · no live replacement: <park>` |
| Run ended | `restart <address> step <step> · <mode> · run is <state>; no live replacement` |
| Still pending (an occupancy wait, a stop in progress, or 30 seconds passed) | `restarting <address> step <step> · <mode> · awaiting launch; visits reset` |

`workflow show` follows a pending restart's progress. History records the canceled live attempt, a full visit reset, and the restart row. Stopping a previous attempt again does not repeat its end or cost. The journal records `{event: "restart", step, continued}` and `{event: "visits_reset", step, outcome: "restart"}`.

### `cancel`

`rimz workflow cancel <RUN>` ends a run as `canceled` and stops its live script, shell command, judge, command step's process group, or every cell of an agent step's layout and its subagents, whether or not the run's definition is missing, unreadable, or refused by validation. It prints `canceled <address>`, and, at a runs step, `its started runs were left running: <ids>` for the started runs that had not ended: a cancel never stops them. An already-finished run id prints `<address> is already <state>; unchanged` and exits 0. Both receipts end with a `workflow show` hint. The checkout's commits and edits, and the run directory, stay. A cancellation journal row may carry `reason`.

An unfinished run holds its worktree against removal and automatic cleanup, even while parked with no agent. `rimz worktree remove NAME --force` removes the checkout and then cancels its unfinished runs with reason `worktree removed`. A failed cancellation leaves the removal standing, exits non-zero, and prints the run id's `rimz workflow cancel` command.

### `resume`

`rimz workflow resume <RUN> [--at <NODE>]` puts a run back to work, at its current node or at `--at <NODE>`, with a fresh timeout clock. A parked run re-enters its node, or `--at <NODE>`; an active run moves only with `--at`. Resume grants nothing: [`extend`](#extend) raises a step's visits or sets a new deadline.

| Refusal | Message |
| --- | --- |
| A definition that does not read, or that validation refuses | the error `workflow advance` reports for it; the run is left as it was |
| `--at` on a try | ``workflow run <id> is a try, which ends at its node and cannot be moved; cancel it with `rimz workflow cancel <id>` `` |
| An unknown node | `unknown workflow node <node>`, as `run --at` refuses it |
| `--at` on a step that spent its visits | `cannot resume at <step>: it has used <used> of <max> visits; grant it another first:`, then the `rimz workflow extend <run> <step> --visits +N` command on the next line, N being the visits that admit one more entry (`+1` unless the step's `max-visits` was lowered below its count) |
| An active run without `--at` | ```an active run needs --at <node> to resume; `rimz workflow extend` changes its bounds``` |
| `--at` while the current attempt is settling | `the attempt at <node> is settling; retry resume after it finishes` |
| A finished run | `workflow run <id> has ended (<state>); cannot resume` |

Every refusal leaves the run as it was. `--at` moves an active run too: a live attempt at its current node ends `canceled` first, and its agents stop, a `keep = true` step's included. Only [`restart`](#restart) resets a visit count, and only the restarted step's, so a step that spent its visits needs an `extend` grant before `--at` sends the run there again. A plain resume of a parked run at a spent step is not refused: the step fails with `visits` at once.

The receipt says `resumed <address> at <node>`, then ` · <node> <used> of <max> visits` when that step is capped, and ` · deadline <clock>` for an armed deadline, and ends with a `workflow show` hint. When the armed deadline has already passed, stderr adds `warning: deadline <clock> has already passed; <destination> — <extend command>`, where the destination reads `the run goes to <target> at once` and the command is `rimz workflow extend <run> --deadline '<when>'`. A fired deadline stays disarmed on resume. The journal records `{event: "resume"}` at the node the run moved to.

```console
$ rimz workflow resume 'probe-wf#main' --at build
error: cannot resume at build: it has used 2 of 2 visits; grant it another first:
  rimz workflow extend 'probe-wf#main' build --visits +1
$ rimz workflow extend 'probe-wf#main' build --visits +1
extended probe-wf#main · build +1 visits, 2 of 3 used
→ rimz workflow show 'probe-wf#main'
$ rimz workflow resume 'probe-wf#main' --at build
resumed probe-wf#main at build · build 2 of 3 visits
→ rimz workflow show 'probe-wf#main'
```

### `extend`

`rimz workflow extend <RUN> [<STEP>] [--visits +N] [--deadline <WHEN>]` records a grant on an unfinished run and nothing else: it enters no node and moves no attempt. An active run advances at once to see the grant; a parked run keeps it for its next resume. Give `--visits`, `--deadline`, or both.

| Flag | Effect |
| --- | --- |
| `--visits +N` | Adds N to one step's `max-visits` for this run. N is at least 1, and the `+` is required: `--visits 2` is refused as `"2" is not a visit grant; give +N, the visits to add to the step's max-visits, with N at least 1`. The step is `<STEP>` when given, else the step the run's session is held or live at, else the run's node. Grants to one step add up, survive restarts and visit resets, and raise no other step. |
| `--deadline <WHEN>` | Replaces the run's deadline and arms it again, a fired one included. `WHEN` is a [workflow time](#workflow-times), resolved when the extend runs: `--deadline 2h` is two hours after it. Any other value exits 2 with the forms. |

`<STEP>` needs `--visits`: a step named with `--deadline` alone is a usage error (exit 2). Grants live on the run record, never in the definition, and `show` includes them in the limit, such as `visits  fix 3 of 5`. `extend` works on a try.

| Refusal | Message |
| --- | --- |
| A step the definition does not have | `<step> is not a step of workflow <workflow>` |
| A named check | `<step> is a check, which has no visits to grant; name a step` |
| `--visits` with no step named while the run is at a check | `` the run is at the check <step>, which has no visits to grant; name a step: `rimz workflow extend <run> <step> --visits +N` `` |
| A step without `max-visits` | `<step> sets no max-visits, so a visit grant changes nothing` |
| A finished run | `workflow run <id> has ended (<state>); cannot extend` |

The receipt says `extended <address>`, then ` · <step> +N visits, <used> of <max> used` for a visit grant and ` · deadline <clock>` for a new deadline, and ends with a `workflow show` hint. A deadline already passed warns as `resume` does. The journal records `{event: "extend", step, visits, deadline}`, each member present only when granted, and `show --history` prints it as `extend · <step> +N visits · deadline <clock>`.

```console
$ rimz workflow extend 'probe-wf#main' --deadline 2020-01-01T00:00:00Z
extended probe-wf#main · deadline 2019-12-31 18:00
→ rimz workflow show 'probe-wf#main'
warning: deadline 2019-12-31 18:00 has already passed; the run goes to handle at once — rimz workflow extend 'probe-wf#main' --deadline '<when>'
```

### Finished runs

A run ends `done` at `@done`, `failed` at `@failed` or on an unrouted failure, or `canceled`. A try ends `done` with the ending `tried` at its node's first answer, and its views print `tried` where another run prints its end state. At the ending RimZ writes the colorless `--history` view to `history.txt` in the run directory and atomically copies it to the notes folder's `history.txt`, then moves the run directory to the room's `finished-workflows/`. When the definition cannot be read at the ending, the saved text keeps every row but leaves out the `→ <node>` targets only the definition names (a turn end's recorded target still prints), after a line `its definition could not be read`, which names the load error unless the run's reason already quotes it. A failed copy, or a move that fails before the rename, warns without failing the transition and leaves the run directory in place, and the next `advance`, `cancel`, or `restart` of that run retries. A failure after the rename leaves the directory finished, and the command says the move completed but its barrier failed. A loop task's own run that ends `done` without acting, while its task still exists, is counted in the task's quiet summary and its directory removed, with no `history.txt` ([quiet runs](../../internals/harness/loops.md#run-reports-and-quiet-runs)). Runs without notes write no copy.

A finished run id still resolves, from any room, and `show` prints it without reading its definition:

- The compact view prints `<address>`, its end state, its reason when the record names one, and `run <run>; the run is finished`, then `work dir <path>` (home-relative, in the finished directory), then the rows of the saved `history.txt`, without that file's opening name, end state, and reason.
- `--history` prints the saved text alone.
- `--json` prints `address`, `dir`, `record`, `journal`, and the saved text as `history`, whatever record version wrote it. `record` is the record as this RimZ decodes it, or as written when another version wrote it or it does not decode.
- Log paths in a finished run's journal and decoded record name the files in the finished run directory, as `dir` does. `prev_attempt.claim_path`, and every run-directory path in a record printed as written, name the run directory as it was while live. A deployed run's `spec_dir` names its deployed folder.

`rimz gc` removes a finished run's directory once its record was last written more than seven days ago, whatever version wrote it, and never removes a live run's directory that holds a record. GC runs about once a day per open room. It backfills a missing notes copy of `history.txt` before removing the directory, and keeps the directory if that copy fails. The notes and history survive run-directory cleanup and default to permanent retention. `rimz reset` keeps both, and `rimz reset --hard` removes both.

### Runs this RimZ cannot continue

A run whose record a newer RimZ wrote is left for that RimZ: `workflow list` skips it, and `show`, `resume`, `extend`, `restart`, and `cancel` by id refuse with `` workflow run <run> has record version <N>, written by a newer RimZ; this RimZ reads only version 19. It is left for the RimZ that wrote it; nothing changed `` and exit 1.

An unfinished run this RimZ cannot continue (an older record version, a missing or invalid one, or a version-19 record that does not decode) is skipped by `workflow list` too. `show` prints its header with why, and `resume`, `extend`, and `restart` refuse with `` this RimZ cannot continue workflow run <run>: <cause>; it reads only record version 19. The workflow pass will end it as failed; nothing changed ``. A `record.json` that holds no JSON object refuses the same way, ending `The workflow pass will set it aside; nothing changed`.

`cancel` and `advance` on such an id end that one run: its record is marked `failed` with why, in place, its agents are released and left running as ordinary sessions, its loop task gets a `failed` report that strikes nothing, and its directory moves to `finished-workflows/`. A record that holds no JSON object moves there unmodified. `cancel` prints `` ended <run> as failed: this RimZ cannot continue it: <cause> `` (or `` set aside <run>: its record holds no JSON object ``), then the agents it left running or `no agent was pinned to it`, and a `workflow show` hint. It ends no other run. When an earlier `cancel` or pass already ended the run but could not release its agents or move its directory, `cancel` and `advance` retry the report, the release, and the move, and `cancel` prints `` <run> was already ended as failed: this RimZ cannot continue it: <cause> `` in place of the first line.

The room's scheduler does the same for every such run on its own, at most once a minute, and one `workflow_ended` notification per pass says how many runs ended and why ([the workflow pass](../../internals/harness/workflow.md#the-workflow-pass)). Until then, background readers skip these runs: the loop scheduler, the start limits' count of open workflow agents, and worktree cleanup.

## Loop tasks

Every fire of a [loop task](./loop.md) is a workflow run of one `test` check named `command`, which runs the task's command in the fire's checkout: exit `0` routes to `@done`, and a failure, a timeout, or an invalid answer to `@failed`. The run keeps its definition in its own `definition/`, not under `~/.rimz/workflows/`, and `rimz workflow show <task>` draws it ([loop task graphs](#loop-task-graphs)). Its address is `<task>#<checkout>`, and `workflow list`, `workflow show <run>`, and `workflow cancel` read it as any other run. The sidebar draws no workflow strip for it: an agent its command launches shows as an ordinary card.

A loop task starts a deployed workflow with `--workflow`, as in `rimz loop add babysit-pr --each-worktree --when pr=open --for 3m --repeat --workflow babysit-pr`. Each fire runs `rimz workflow run <name>` with the task's arguments, and the run it publishes is the task's own: it carries the task's name and the fire's id, the fire's outcome is the run's outcome, recorded when the run ends, and `loop stop`, the task's overlap policy and its launch memory reach it. A fire whose `workflow run` refuses before it publishes a run is recorded `error` and counts a strike. `loop add` refuses `--each-worktree` and `-w` for a project-scoped workflow. A `rimz workflow run` that a shell task's command runs is an ordinary run: it carries the fire's id, so `loop show` lists it and `loop stop` stops it, but it reports to nobody.

`loop show` lists the fire runs the task started, finished runs included. `loop logs` gives each fire block its run's state and `workflow show` command, and `loop fire` prints that command under its `launched` receipt. `loop list` keeps only active runs.

## Failure text

Script process failures say `<script> exited N` or `<script> killed by SIGTERM` (the signal's name when known). `<script>` is the first `run` word containing `/`, or its first word when none does. A judge whose session ends without an answer says `the judge's session ended without an answer` unless its launch named another reason. A run keeps the text it recorded when it ran.

## Assists

Routes, step launches, parks, and ends append `workflow_*` assists ([stats](./stats.md#the-assist-timeline)). A loop fire whose command check passes appends none. A merged wait appends one route assist when it closes, with its final `count`. An open wait has no route assist yet. Stats renders a closed wait as `workflow <workflow> <node> waited <outcome> ×<count>`.

## JSON shapes

`list --json` and `show <workflow> --json` print arrays of `{id, address, workflow, scope, state, node, since, detail}`, with optional `hold` for a known capacity cause, `once: true` for a try, and `starter` for a run with a starter: the record's `starter` plus `handle` while the starting agent is in the room. `list --all --json` appends the finished runs to the same array, unfolded and at most `-n` of them, each in that shape with an empty `detail` and an `ended` key (RFC 3339). `scope` is the scope path: the checkout, or the project root for `scope = "project"`. `since` is node entry, not the human row's state-specific age. `detail` is the [waiting line](#the-waiting-line), so its capacity bounds follow the same day-qualified clock as rechecks and passed deadlines. Read the node and its entry time from `node` and `since`, never from `detail`.

`show [RUN] --json` prints `{address, dir, record, journal}`: the short run address, work directory, run record, and journal entries. Runs with kept notes add `notes`, the absolute folder path, and runs with a starter add `starter`, as in `list --json`. A live run at an agent, message or runs step adds `sign`, the absolute path of the step's sign file, on an `asked` park too. When the definition cannot load, it still prints the record and journal, without `sign`. A finished run adds `history` ([finished runs](#finished-runs)).

### The record

| Field | Meaning |
| --- | --- |
| `record.version` | `19`. RimZ refuses a record of any other version ([runs this RimZ cannot continue](#runs-this-rimz-cannot-continue)). |
| `record.once` | `true` for a try (`run --once`), omitted otherwise. |
| `record.starter` | Who started the run: `{by: "agent", kind, launch}` or `{by: "run", run, node}`. Omitted for a start by hand and a loop task's run. |
| `record.depth` | The run's [depth](#starters-and-depth), `1` when omitted. |
| `record.loop_report` | `owed` while the run owes its loop task, starting agent or starting `runs` step its outcome report, `sent` once made, `none` when nobody is owed one. |
| `record.outcome.ending` | How a finished run ended: `{kind}` with `kind` one of `done`, `failed`, `canceled`, `tried`, or `capacity_gave_up` (with `waited_ms` and optional `reason`). |
| `record.spec_dir` | A deployed run's definition folder, canonical at the start. Absent for a loop task's run, which reads its own `definition/`. |
| `record.visits.<step>` | The step's visit count since its last manual restart. |
| `record.key_visits.<step>` | The step's visits split by item key, as `key_visits` in [the context file](#the-context-file). Absent when no step has a keyed visit. |
| `record.prev_attempt` | As in [the context file](#the-context-file). |
| `record.restart` | `{resume: "session-id" \| null, settled?: N}` while a manual replacement is pending. Null means a fresh start. `settled` names a previous attempt being stopped again whose end and cost are already journaled. It disappears when the launch is published, when a route leaves the step for another node (a capacity give-up, a deadline) or `resume --at` moves the run, or when the run parks or terminates. |
| `record.held` | While checking an open session: `{attempt, entered_at, verdict?}`, the session attempt, the step's entry time, and the saved check verdict. |
| `record.abandoned` | Judges whose evaluation ended while their agent still ran (an interrupt of the held session they judged), owed a stop. The next engine pass stops them, and a run that ends stops them as it ends. Omitted when empty. |
| `record.wait` | The open wait: `node`, `detail`, `first`, `last`, `count`, and for a condition `recheck_at`, `case` during a `for` hold (omitted without one), and `unknown` (the reason of an `unknown` answer routed to `@wait` during a hold, cleared by a true answer of the held case). |

A capacity wait has `record.wait.outcome = "capacity"`, provider `kinds`, optional `hold` (the [typed cause](../../internals/harness/loops.md#the-start-throttle)), and optional `gives_up_at` (absent for `forever`, and when neither the node nor the run bounds the wait). Closing the wait carries `hold` into its history entry.

A verdict is `{node, entered_at, at, target, object, reason}`, where `at` is the instant its evaluation was journaled. A closing attempt carries the verdict in optional `attempt.ending.route` until the stop settles.

### Attempts

| Field | Meaning |
| --- | --- |
| `attempt.visit` | The visit count at admission, including that launch or push-back. The context, `live_attempt`, `prev_attempt`, and the attempt journal row carry it. An attempt without one uses the current count. |
| `prev_attempt.routed_by` | The outcome that admitted the attempt, beside `key`, null when absent. Also on attempt journal rows. A live `attempt` carries neither field: the routing outcome is in `record.input`. |
| `attempt.asking` | An open blocking ask: `{since, text, kind, handle}`, `kind` one of `question`, `permission`, or `plan_approval`. |
| `attempt.asked_ms` | The total closed pause time in milliseconds. |
| `attempt.held_since` | Opens the shared pause at the earliest ask, limit, error, or budget-park evidence, clamped to the attempt's start and the observation time. It stays open through overlaps, and `asked_ms` credits the interval once all such evidence clears. |
| `attempt.lifecycle` | `session`, omitted for turn attempts. A session attempt is `held.attempt` while checking, otherwise `attempt`. |
| `attempt.push` | `{message, queued}`: the push-back message id, persisted before delivery. `queued` means the host has confirmed enqueue, including an id already in live or retained history. |
| `attempt.judged` | `{agent_id, started_at, ended_at}`, the consumed leader turn end. |
| `attempt.started_past_limit` | The `--skip-capacity` hold text. |
| `attempt.answer` | A judge's accepted `submit` answer, normalized against its check. The engine routes on it alone. Omitted until one is accepted. |
| `attempt.reminded` | `true` once a judge that rested without an answer was reminded. Omitted while false. |
| `attempt.evidence` | A judge's staged ending evidence: its failure's detail, with a `reason` naming an errored turn's text or what a timeout found it stopped on. A judge's ending leaves `prev_attempt` the last step's. |
| `code`, `log` | A command step's exit code (null after a signal or an unobserved exit) and the absolute `work/<seq>.step.log` path when the file exists. Null for agent attempts. |
| `message` | A message step's queued message id, also on its journal `attempt` row. Other attempts omit it. |

`asking`, `asked_ms`, and `held_since` pause a step's timeout, not the run deadline. A judge records them too, for the views that name its stop, and its timeout never reads them. They are omitted when empty or zero. Command steps have null `handle`, `run_id`, `children`, `asked`, and `cost_usd`. A message-step attempt carries `handle` (the target, null when undelivered) and `message`, with `code`, `log`, and `run_id` null. The hosting process samples `head_start` before a command and `head_end` after reaping it. `claim_path` names `work/<seq>.claim.json`.

### Journal rows

- Evaluation rows carry `origin = "check"` for a check's answer or `"engine"` for an engine-decided outcome.
- A forwarded signal journals `{event: "signal", node: <step>, signal: <name>, message: <message id>}`, with the subscribing step as its node even while a session is held at its rest check.
- Each turn-end decision or held-wait answer writes `{event: "turn_end", seq, node, action, turn, check}`: `action` is `"kept"`, `"pushed"`, `"closed"`, `"discarded"`, or `"interrupted"`, and `turn` is `{agent_id, started_at, ended_at}`. A saved verdict supplies optional `outcome` and `target`, and `reason` explains an interruption or a moved-turn discard. A `pushed` row also carries optional `visit` and `message`, and counts as a visit in history, the trail, and the visit counts.
- A plain sleep journals `{outcome: "sleeping", recheck_at: "<entered_at + sleep>"}` at entry and `{outcome: "then", reason: "slept <sleep>"}` at expiry, inheriting the key into each object. A conditional sleep uses condition answers with `case: 0` for an early win: false is `sleeping`, unread is `unknown`, and an unfinished `for` rewrites a win to `hold`. These answers never replace the run's input or key. History shows `sleeping → @wait` and `then → <then>`.
- A start given `--at` or `--input` writes `{event: "start", node, input?}` as the journal's first row, `input` being the `--input` object.
- A try's terminal row carries `tried`, the target of the route it did not take (`@done`, `@failed`, `@wait`, or a node).
- A restart writes `{event: "restart", step, continued}` and `{event: "visits_reset", step, outcome: "restart"}`.

### Loop JSON

`rimz loop list --json` carries `workflow_runs` for a task: one object per unfinished run the task started in that room, `{id, address, checkout, state, node, since}` plus `reason` and `text` on a parked run. Both list and show add the shared `detail` when nonempty. `since` is node entry. A human parked summary's age starts at the park.

`rimz loop show --json` lists, unfolded and unlimited, every run named by the task's launch memory first, including finished runs whose task was renamed, then every other run the task started in that room, live ones first and finished ones newest first, each once. A finished run adds `terminal_at` (RFC 3339). A run canceled while parked keeps its recorded `reason` and `text` in JSON, without appearing as parked in human output. A run whose record is gone has only `id`, `checkout` (the launch entry's key), `state: "gone"`, and `since` (the launch time). Fan-out `worktrees[].run` repeats the matching `workflow_runs` object. Project-scoped launches share one run across checkout rows. The human WORKTREES table names those runs, and RUNS lists only runs no worktree row names, folding adjacent finished ones ([`loop show`](./loop.md#loop-show)).

## Exit codes

| Code | When |
| --- | --- |
| `0` | Success, including a `wait` whose runs ended `done`, `cancel` of an already-finished run, `show <WORKFLOW>` with an unavailable definition, and `validate` with warnings only. |
| `1` | A refusal or failure: an unknown or ambiguous run, a duplicate start, a terminal resume, extend, or restart, a refused `submit`, a validation error, an unknown node, a try that ended other than `tried` under `run --once`, a start past the depth limit, a `wait` on a run that ended `failed` or `canceled`, `show` with no RUN outside a workflow, and a node view of a finished run. |
| `2` | A usage error: a clap parse error, a `wait` on an unknown run id or a bare workflow name, a malformed `<workflow>#<checkout>` address, `show <run> <NODE>` with an unfinished run's address or id, `--history` with a workflow name, `--history` or a NODE together with `--json`, and a name that is both a deployed workflow and a loop task without a `workflow:` or `task:` selector. |
