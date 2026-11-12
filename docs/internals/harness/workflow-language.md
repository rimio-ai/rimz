# Workflow language target

This page specifies a target: the workflow definition language and the loop command that RimZ is being rebuilt toward, not the behaviour RimZ ships. The [workflows guide](../../guide/workflows.md), the [workflow CLI reference](../../reference/cli/workflow.md) and the [loop reference](../../reference/cli/loop.md) describe the shipped product, and they move link by link as each build link merges. Every build link builds to this page, and two of them need nothing from it: the compatibility removal and the typed pipeline. A link that finds a meeting point this page leaves unowned asks the owner before it builds one. The page was written for the parallel build of ten links in separate worktrees, so it states each rule whole, names the link that owns it, and carries worked examples the links use as acceptance cases.

The goal is a language whose every rule fits in one sentence without "except", and a loop command that owns only when a run starts. Each rule below is numbered (R1 to R49) so a link can cite it. Whatever this page does not name keeps its shipped meaning and stays owned by the [workflow CLI reference](../../reference/cli/workflow.md): the check answer object and `context.json` apart from the failure input, `key`, `recheck_at`, signs, `wake` and `subscribe` entries, launch settings, notes, the layout grammar, and the run control commands apart from `resume --at` and the count reset `resume` applies after a cap (the note under R40). The condition grammar and its keys stay owned by the loop reference's [conditions](../../reference/cli/loop.md#conditions) section. Every rule is a target that no build has run yet. The page ends with two lists for the owner: what it decided beyond the ten picks, and each rule that still carries an exception.

## Definition file

- R1. A node is a table named by its kind, `[<kind>.<name>]`, and a name is unique across all eight kinds.
- R2. Work nodes act (`agent` launches agents, `command` runs a command, `message` queues a message for a living agent), checks decide (`test` answers `pass` or `fail` from an exit code, `script` answers with a JSON object, `judge` is a headless agent that answers, `condition` answers from ordered cases), and `sleep` waits.
- R3. A `command`, `test` or `script` node gives its command as `run` (an argv whose path words stay inside the definition directory) or `shell` (`sh -c`).
- R4. A route is a target: a node name, `@done`, `@failed`, or, from a check, `@wait`, and route tables (`to`, `delay`, `default`), `@user` and `@break` do not exist.
- R5. `then` is where the run goes when a work node or a sleep completes, `on.<outcome>` is where a check's answer goes, `on.<cause>` and `on.failed` are failure routes, and `on.asked` is where an agent's blocking question goes.
- R6. A check routes every outcome it can answer: a test `pass` and `fail`, a condition every case outcome, its `else` outcome and `unknown`, and a script or a judge the outcomes its `on` names, any other answer being the failure `invalid`.
- R7. An author's outcome name is never a reserved word (the eight causes, `failed`, `asked`, `unknown`): a condition case's `outcome` or `else` that is one refuses, and a reserved key in an `on` table is the engine's route (a failure route, `on.asked`, a condition's `on.unknown`), so a script or judge that returns a reserved word has given an answer no outcome names and is `invalid` (R6).
- R8. An unknown field refuses, an invalid value refuses, and a field belongs to the tables the field table lists it under.
- R9. Every cycle is bounded: a cycle that contains a work node contains a work node with `max-visits`, a cycle that contains none contains a sleep or a check with a `@wait` route, and failure routes count as edges.
- R10. A `visits` or `capacity` failure is raised before its node acts and counts no visit, so its route (after the fallback chain) must lead where that node cannot be reached again.
- R11. A run seals its definition and every file the definition names at its start, stamped with the binary's language version; a binary with another language version refuses the run, and a build link that changes what a sealed definition means raises the version.
- R12. A start that names a worktree for a workflow with `scope = "project"` refuses.

The field table. A `*` marks a required field. Every node also takes `summary`, `on.failed`, and `on.<cause>` for each cause R14 lists for its kind.

| Table | Fields |
| --- | --- |
| `[workflow]` | `summary`, `scope`*, `start`*, `deadline`, `on.failed`, `limit` (`max-parallel`, `stagger`) |
| `[agent.<n>]` | `agent`*, `lifecycle`*, `then`*, `prompt-file`, `on.asked`, `max-visits`, `timeout`, `keep`, `notes`, `admission` (`window`, `occupancy`, `max-parallel`, `stagger`, `throttle`, `give-up`), `account`, `mode`, `effort`, `fast`, `budget`, `system-prompt-file`, `worktree`; on a session also `rest-check`, `subscribe` |
| `[command.<n>]` | `run` or `shell`*, `timeout`*, `then`*, `max-visits` |
| `[message.<n>]` | `message`*, `then`*, `prompt-file`, `session`, `max-visits` |
| `[test.<n>]` | `run` or `shell`*, `timeout`*, `on.pass`*, `on.fail`*, `wake` |
| `[script.<n>]` | `run` or `shell`*, `timeout`*, `on.<outcome>`*, `wake` |
| `[judge.<n>]` | `agent`*, `prompt-file`*, `timeout`*, `on.<outcome>`*, `wake`, `admission` (`window`) |
| `[condition.<n>]` | `cases`* (`when`, `outcome`, `for`), `else`*, `timeout`*, `on.<outcome>`*, `on.unknown`*, `wake`, `provider`, `account` |
| `[sleep.<n>]` | `sleep`*, `then`*, `when`, `for`, `provider`, `account` |

Shipped fields with no place in the table, and where each went: `on-break` and `workflow.on.deadline` (the fallback chain, R13); `takeover` and `share` (`admission.occupancy`); `window`, `throttle`, the step `limit` and `capacity-wait` (the `admission` table, where `capacity-wait` is `give-up`); `on.cap` (`on.visits`); `max-visits-per-outcome` with `reset-on` (gone, R40); route `delay` and `default` (gone); lifecycle values `stay` and `until` (`session`); a `then` list (`rest-check`). `else` and `timeout` are required on a condition because R6 requires every outcome routed and R27 requires every wait without an agent pane to be bounded.

The shipped tables are `crates/rimz/src/harness/workflow/spec.rs::Header`, `crates/rimz/src/harness/workflow/spec.rs::Check`, `crates/rimz/src/harness/workflow/spec.rs::Step` and `crates/rimz/src/harness/workflow/spec.rs::StepEdges`. A judge check reads neither `account` nor `provider` today (only a condition or a sleep does, in `crates/rimz/src/harness/workflow/script/condition.rs::reading`), so `[judge.<n>]` takes neither.

## Failures

- R13. A failure takes its own `on.<cause>` route if the node sets one, else the node's `on.failed`, else the workflow's `on.failed`, else the run fails.
- R14. The causes are `died` (the node's process could not start or ended abnormally), `invalid` (an answer the node cannot use), `timeout` (R26), `limited` (a budget or provider refusal that ends the node), `undelivered` (a message found no single living agent), `visits` (the entry would exceed `max-visits`), `capacity` (the start gave up waiting for capacity, or can never fit) and `deadline` (the run's deadline passed), and they occur at: agent `died`, `timeout`, `limited`, `visits`, `capacity`, `deadline`; command `died`, `timeout`, `visits`, `deadline`; message `undelivered`, `visits`, `deadline`; test `died`, `timeout`, `deadline`; script `died`, `invalid`, `timeout`, `deadline`; judge `died`, `invalid`, `timeout`, `limited`, `deadline`; condition `timeout`, `deadline`; sleep `deadline`.
- R15. A dead agent never follows `then`: a turn agent died when its process exits before a clean turn end, a session agent died when its process exits while a turn is open or before its first turn, and a session that exits at rest ended cleanly.
- R16. A blocking question takes `on.asked` when the node sets it, on either lifecycle, and otherwise stays open in the agent's pane with the node's clock paused.
- R17. Every failure route hands the next node the input `{ outcome: "failed", cause, node, reason, detail }` with the run's `key` inherited, where `detail` holds the cause's own facts (a limit's `reset_at`, a command's exit code and log path, the time waited, the cap).
- R18. The deadline fails the node the run is at, once per grant.
- R19. A judge that fails runs once more before its failure is raised, and an agent launch that failed before it started anything is tried once more.
- R20. `rimz workflow resume <run> --at <node>` moves a live run to a node, which is what lets an author's handler node send the run on.

R15 rests on a premise no build has run: that a session's death is detectable at exit, from the supervised run's turn stamps, which `crates/rimz/src/harness/run.rs::settle_session_exit_with_spend` does not read today. If the premise holds, the failures link builds this definition; if it fails, the definition goes back to the owner.

R20 differs from shipped: today `crates/rimz/src/harness/workflow/engine.rs::resume` takes `--at` on any parked run, and an active run only while it is at the generated handler.

Differs from shipped, for R13 to R19:

- A command's non-zero exit, signal or lost host is `died` and does not follow `then`. Today `crates/rimz/src/harness/workflow/engine.rs::command_step` ends a non-zero exit or a signal `died`, `crates/rimz/src/harness/workflow/engine.rs::prepare` reclaims a lost host as `canceled`, and `crates/rimz/src/harness/workflow/graph.rs::Definition::target` sends a step's `ended`, `died`, `canceled`, `limited` and `timeout` through `then` unless `on.failed` is set and the ending is `died`, `timeout` or `limited`.
- An agent launch failure is `died`. Today the engine routes it to the step's `invalid` edge (the launch failure branch in `crates/rimz/src/harness/workflow/engine/agent.rs`).
- A script's non-zero exit is `died`. Today `crates/rimz/src/harness/workflow/script.rs::evaluate` turns it into an unusable answer, `invalid`.
- A judge's budget refusal is `limited` and routable. Today `Definition::target` sends a check's `budget` outcome to `@failed`, hardwired.
- A start too large for its cap is `capacity` and routable. Today `crates/rimz/src/harness/workflow/engine.rs::prepare` ends the attempt `died` with a route to `@failed`.
- An expired capacity wait takes the whole chain. Today `capacity` is not among the edges that fall back to `on-break` (`crates/rimz/src/harness/workflow/graph.rs::FAILURE_EDGES`).
- A session that crashes mid-turn is `died`. Today `settle_session_exit_with_spend` settles any non-pending session as completed, without reading the exit.
- A run with no failure route fails without an `on-break` line.

## Lifecycles

- R21. An agent node names its lifecycle, `turn` or `session`, and there is no default.
- R22. A turn node is done at its leader's clean turn end and follows `then`.
- R23. A session node runs the first check of its `rest-check` list each time its leader comes to rest; from a rest check, a route to the session's node sends the answer into the session and counts a visit, `@wait` keeps waiting, a route to another check of the list continues the rest there, any other route closes the session and goes there, failure routes from a rest check follow the same rule, and `then` is where the run goes when the agent exits on its own.
- R24. The run stays at the session node while its rest check runs.
- R25. `keep = true` leaves the node's agents open, outside the run, when the node ends cleanly or its rest check routes away, and a failure or a cancel still stops them; a node whose layout names a team is a session with no rest check, and `subscribe` is a session's field.

## Timeout

- R26. `timeout` is how long the run may stay at the node on one entry, passing it fails the node with cause `timeout`, and the clock of an agent (an agent node's or a judge's) is paused while that agent cannot work (a question, a provider limit or error, a budget park).
- R27. The clock starts when the node's start is admitted, with the waits for a usage window, a checkout and capacity sitting in front of the node; while a session's rest check runs, the session's clock and the check's clock both run and the first to pass fails its own node; and `timeout` is required on `command`, `test`, `script`, `judge` and `condition`, optional on `agent`, and not a field of `message` or `sleep`.

Today one function, `crates/rimz/src/harness/workflow/inspect.rs::timeout_at`, sets the clock: at a check from the check's entry, at a step from the attempt's start plus its asked time, and none while a hold or an ask is open.

## Conditions

- R28. A key reads one of its values, reads `none` when a fresh look found nothing there, or is unread; a term over an unread key is unknown, and `!`, `&&` and `||` are three-valued (true `||` unknown is true, false `&&` unknown is false), with no expression-wide veto for an unread key.
- R29. `unknown` is not a value a term can match: `key=unknown` refuses in every condition.
- R30. The freshness floors and the CI fence decide whether a key counts as read, and this page does not change them.
- R31. A `for` hold runs while its condition stays true, survives unknown and ends on false, in a loop row and in a workflow alike.
- R32. A condition node answers its first true case, `unknown` when an unknown case comes before any true one, and `else` when every case is false, and a case with `for` answers once its condition has held that long, the node waiting until then.
- R33. Each reader treats unknown its own way: a loop trigger fires on true alone; loop fan-out and `rimz loop list` show `unknown`; `rimz loop explain` prints the three values and exits 0 on true alone; a manual fire reports unmet or unknown and proceeds; an admission `window` admits on true alone; a sleep's `when` wakes it on true alone; the cache keep-alive stops on false alone.
- R34. `rimz wait` and `rimz message --when` use another evaluator and are outside this page.

Today `crates/rimz/src/harness/when.rs::ForgeKey::values` lists `none` for `pr.queue` and `pr.review` and the literal `unknown` on every key, and `crates/rimz/src/harness/when.rs::ForgeKey::read` returns no reading both for a branch with no pull request and for a key never read; R28 extends `none` to every forge key. A workflow condition or sleep that matches `unknown` already refuses in `crates/rimz/src/harness/workflow/validate.rs`, and R29 makes that the rule for every condition.

## Capacity and visits

- R35. Every start waits in front of its node for its usage window, its checkout and capacity, a run started by `rimz workflow run` waits too, and its receipt says when the run is held.
- R36. An agent node's `admission` table holds `window`, `occupancy` (`wait`, the default, `takeover`, or `share`), `max-parallel`, `stagger`, `throttle = "off"` and `give-up` (a duration from the first capacity hold), and a judge's holds `window`.
- R37. A capacity wait gives up at the node's `give-up`, else at the run's give-up instant, else never, and giving up raises `capacity`.
- R38. A start given `capacity = "skip"` (`--skip-capacity`) launches past a capacity hold and records the hold it skipped.
- R39. `[workflow.limit]`, the machine `[throttle]` configuration and the loop's pacing keep their scopes.
- R40. A work node's `max-visits` bounds its entries, where an entry is each time the run enters the node to act (an agent launch, a command run, a message send, or an answer sent back into a session), and nothing a definition says and no route or answer resets or adds to a count.

How R40 meets the shipped count and the run control commands:

- Kept from shipped: every work node kind counts, at admission, before it acts (`crates/rimz/src/harness/workflow/engine.rs::prepare` bumps the count for any step). An undelivered message send still counts. A launch retried under R19 is one entry.
- Kept from shipped, as run control commands: `rimz workflow restart` starts its node's count over (`crates/rimz/src/harness/workflow/state.rs::Transition::restart_step`), and `rimz workflow resume --extend visits=+N` raises every cap for the run (`crates/rimz/src/harness/workflow/engine.rs::resume`). These are an operator's acts on a run, not something a definition says.
- Differs from shipped: the count reset `resume` applies when it resumes a cap park or leaves the generated handler after a cap (both in `crates/rimz/src/harness/workflow/engine.rs::resume`) has no case left. A run parks only by a route to `@user` (`crates/rimz/src/harness/workflow/state.rs::Transition::route`), which R4 removes, and the generated handler is gone (R13). An author's handler that sends a run back to a spent node passes `--extend visits=+N` with `resume --at`; without it `visits` is raised again at once.

The last point rests on a premise no build has run: that once the compatibility removal has merged and R4 holds, no run parks. The only arm that sets the parked state is the `@user` target in `Transition::route`, and a current definition that names `@user` refuses (`crates/rimz/src/harness/workflow/spec.rs::reject_reserved_target`); not traced is every default edge that fills `@user` outside the legacy routing mode. If a park survives, the failures link keeps the parked branch's reset and asks the owner.

Today `crates/rimz/src/harness/workflow/engine.rs::prepare_capacity` lets a start wait for capacity only for a run with a loop task that is neither manual nor a restart, with the step's `capacity-wait` from the first hold as its give-up time, else the run record's cutoff, which `crates/rimz/src/harness/schedule/runner.rs::TaskFire::default_capacity_cutoff` computes for a scheduled inline row that sets no `capacity-wait`.

## Loop

- R41. A row holds a trigger, placement and fan-out, overlap, pacing, strikes, the start arguments and one action: `workflow = "<name>"`, an inline node, or delivery to a waiting session.
- R42. A fire starts a run of the row's workflow or inline node with the row's start arguments, or delivers the row's prompt to its waiting session.
- R43. An inline node is a node table of this language, `[tasks.<task>.agent]` or `[tasks.<task>.command]`, run as the workflow `{ scope = "worktree", start = <that node> }`, whose name is its kind word and whose routes name `@done` or `@failed`.
- R44. The start arguments, shared by `rimz workflow run` and a row, are where the run happens (a checkout, or a fresh worktree per start), the prompt, the give-up instant and `capacity`; a row spells them `prompt` or `prompt-file`, `fresh-worktree`, `give-up` and `capacity`, and `rimz workflow run` spells them `--prompt`, `--fresh-worktree`, `--give-up` and `--skip-capacity`.
- R45. `rimz loop add --agent` or `--command` with its option flags is a constructor that writes the inline node and adds nothing the stored row does not show: `--lifecycle` is required, an inline node's occupancy is the one `--occupancy` names, `wait` when the flag is omitted, and `timeout` is written when the row is added (from `--timeout`, else `loop.default-timeout`, else 2h).
- R46. An agent node's prompt is its `prompt-file`, its sign files and the run's prompt, in that order, each when present, and a start that would launch an agent with none refuses.
- R47. Every loop-started run receives a give-up instant: the row's `give-up`, else one second before the row's next clock fire capped by the row's deadline, else the deadline, else none, and a row that names a workflow gets it like an inline one.
- R48. `rimz loop eject` moves the row's inline node into a workflow file and leaves `workflow = "<name>"` on the row, with the start arguments staying on the row.
- R49. A row with `each-worktree` that names a workflow with `scope = "project"` refuses when it is added.

Row fields after R41:

- Trigger: `at`, `fire-at`, `every`, `cron`, `signal`, `match`, `when`, `for`, `watch`, `once`, `deadline`, `provider`.
- Placement and fan-out: `root`, `dir`, `label`, `each-worktree`, `once-per-checkout`.
- Overlap: `overlap`, `overlap-limit`.
- Pacing: `budget-per-day`, `surplus`, `surplus-after`.
- Strikes: `max-strikes`.
- Start arguments: `prompt`, `prompt-file`, `fresh-worktree`, `give-up`, `capacity`.
- Action: `workflow`, `[agent]`, `[command]`, or `wait` with `wait-meta`, `team`, `loop-task`.

Fields leaving the row (today's row is `crates/rimz/src/config/loop_.rs::TaskEntry`): `stay`, `takeover`, `check`, `check-timeout`, `judge`, `verify`, `max-attempts` and `on` go with the guards; `agent` as a string, `subscribe`, `notes`, `worktree`, `mode`, `effort`, `fast`, `budget`, `system-prompt-file`, `timeout`, `account`, `throttle` and `capacity-wait` each become a field of the inline node or a start argument.

## Worked examples

Each example is an acceptance case: a link that owns a rule the example uses builds until the example means what its note says.

### Example 1: the committed babysitter

Source: `crates/rimz/tests/fixtures/workflows/babysit-pr/workflow.toml`.

```toml
[workflow]
scope = "worktree"
start = "pr"

[script.pr]
run = "uv run checks/pr.py"
timeout = "2d"
wake = ["ci.*", "pr.*"]
on = { merged = "notes", ci-pending = "@wait", needs-sweep = "sweep" }

[script.notes]
run = "uv run checks/notes.py"
timeout = "5m"
on = { none = "@done", needed = "triage", unsure = "judge" }

[judge.judge]
summary = "Decide whether the PR needs triage"
agent = "haiku"
prompt-file = "checks/triage-judge.md"
admission = { window = "5h.left>=42" }
timeout = "3d"
on = { skip = "@done", triage = "triage" }

[agent.sweep]
agent = "sweeper"
lifecycle = "turn"
prompt-file = "prompts/sweep.md"
admission = { occupancy = "takeover" }
max-visits = 5
timeout = "6h"
then = "pr"
on = { died = "pr", timeout = "pr", limited = "pr" }

[agent.triage]
agent = "yagni"
lifecycle = "turn"
prompt-file = "prompts/triage.md"
admission = { window = "5h.left>=42", occupancy = "takeover" }
max-visits = 3
then = "notes"
on = { asked = "judge", died = "notes", limited = "notes" }
```

Differs from shipped: there is no `on-break = "@failed"` line, since R13 ends in "the run fails". `notes` gains a timeout (R27). A sweep that died, timed out or was limited, and a triage that died or was limited (it sets no `timeout`), return to their checks through explicit routes, where today every such ending follows `then`. A spent visit cap or an expired capacity wait fails the run. A catch-all `on.failed = "pr"` on `sweep` would refuse here (R10): `visits` and `capacity` count no visit, so that route would reach `sweep` again through `pr` without bound. `checks/pr.py` is unchanged: it reads `prev_attempt`, which keeps its shape.

### Example 1b: the guide's babysit recipe

Source: the "Wait on the outside world" section of `docs/guide/workflows.md`.

```toml
[workflow]
scope = "worktree"
start = "pr"

[condition.pr]
summary = "Wait for merge or sustained CI trouble"
timeout = "2d"
cases = [
  { when = ["pr=merged"], outcome = "merged" },
  { when = ["pr.queue=queued"], outcome = "queued" },
  { when = ["ci=failed || pr.queue=dequeued"], outcome = "red", for = "2m" },
]
else = "pending"
on = { merged = "@done", queued = "@wait", red = "sweep", pending = "@wait", unknown = "@wait" }

[agent.sweep]
summary = "Repair CI and re-arm the PR"
agent = "claude"
lifecycle = "turn"
prompt-file = "prompts/sweep.md"
admission = { occupancy = "takeover" }
max-visits = 5
timeout = "6h"
then = "pr"
on = { died = "pr", timeout = "pr", limited = "pr" }
```

Differs from shipped: `max-visits-per-outcome = { limit = 2, reset-on = "queued" }` is gone, so the recipe loses "two sweeps per red streak" and keeps its total of five (R40). `unknown` is routed explicitly (R6).

### Example 2: the guide's fix-tests

Source: the first workflow in `docs/guide/workflows.md`.

```toml
[workflow]
scope = "worktree"
start = "tests"

[test.tests]
shell = "cargo test"
timeout = "30m"
on = { pass = "@done", fail = "fix" }

[agent.fix]
agent = "claude"
lifecycle = "turn"
prompt-file = "prompts/fix.md"
max-visits = 3
timeout = "1h"
then = "tests"
```

Differs from shipped: the suite has a timeout. A fixer that dies or times out fails the run instead of returning to the suite through `then`.

### Example 3: the guide's fix-and-reflect

Source: the "Reflect before a failed ending" section of `docs/guide/workflows.md`.

```toml
[workflow]
scope = "worktree"
start = "result"
deadline = "2027-01-15T18:00:00Z"
on.failed = "reflect"

[script.result]
run = "sh checks/status.sh"
timeout = "5m"
on = { retry = "fix", done = "@done", broken = "reflect" }

[agent.fix]
agent = "claude"
lifecycle = "turn"
prompt-file = "prompts/fix.md"
max-visits = 3
timeout = "1h"
then = "result"

[agent.reflect]
agent = "reflector"
lifecycle = "turn"
prompt-file = "prompts/reflect.md"
timeout = "10m"
then = "@failed"
on.failed = "@failed"
```

Differs from shipped: the script's `failed` answer is renamed `broken` (R7). The workflow's `on.failed` replaces `on.deadline`, the check's `invalid` route and `fix`'s `on.cap`. The `finish-failed` check disappears because `then = "@failed"` ends the run. `reflect` needs no visit cap because its own `on.failed` keeps it off every cycle. A fixer that dies or times out goes to `reflect` instead of back to `result`.

### Example 4: a guarded agent

Source: the loop compiler's shell guard with the default polarity, declining by ending the run.

```toml
[workflow]
scope = "worktree"
start = "check"

[test.check]
shell = "scripts/has-work.sh"
timeout = "5m"
on = { pass = "@done", fail = "agent" }

[agent.agent]
agent = "claude"
lifecycle = "turn"
timeout = "2h"
then = "@done"
```

with the row that starts it:

```toml
[tasks.triage]
cron = "0 9 * * *"
workflow = "guarded-agent"
prompt = "Triage what the check found."
```

Differs from shipped: today `rimz loop add --check` compiles this graph into the run at each fire (`crates/rimz/src/harness/schedule/compile.rs::Graph`), with the guard's `timeout` and `invalid` routed to `@failed`. Here the author writes the workflow file, and the row names it and carries the prompt as a start argument (R44).

### Example 5: a judge with a recheck in front of a session

Source: the loop compiler's judge, its recheck sleep and its `--stay` agent.

```toml
[workflow]
scope = "worktree"
start = "judge"

[judge.judge]
agent = "haiku"
prompt-file = "checks/judge.md"
timeout = "5m"
on = { pass = "recheck", fail = "agent", died = "agent", invalid = "agent", timeout = "agent" }

[sleep.recheck]
sleep = "30m"
then = "judge"

[agent.agent]
agent = "claude"
lifecycle = "session"
admission = { occupancy = "share" }
then = "@done"
```

Differs from shipped: "run the agent anyway" when the judge fails is three explicit routes, not `on.failed`, which would also send a `limited` judge and the deadline to the agent. The session shares its checkout because the definition says so, where the compiler set `share` whenever `--takeover` was absent.

The agent sets no `prompt-file` because the shape's prompt is the row's, so the run is started by a row that sets `prompt`, as in example 4, or by `rimz workflow run --prompt` (R44, R46), and a start that passes none refuses.

### Example 6: a verify loop

Source: the loop compiler's `--verify`.

```toml
[workflow]
scope = "worktree"
start = "agent"

[agent.agent]
agent = "claude"
lifecycle = "session"
rest-check = ["verify"]
max-visits = 3
timeout = "2h"
then = "verify"

[test.verify]
shell = "cargo test"
timeout = "5m"
on = { pass = "@done", fail = "agent" }
```

`verify` is both the session's rest check and its `then` target. At a rest, `fail` sends the answer into the open session; after the agent exits on its own, `fail` launches it again; either way the entry counts a visit (R23, R40).

The agent sets no `prompt-file` because the shape's prompt is the row's, so the run is started by a row that sets `prompt`, as in example 4, or by `rimz workflow run --prompt` (R44, R46), and a start that passes none refuses.

### Example 7: two inline rows

Source: the loop compiler's `--agent` and a bare `--check`.

```toml
[tasks.nightly]
cron = "0 3 * * *"
prompt = "Triage the new issues."

[tasks.nightly.agent]
agent = "claude"
lifecycle = "turn"
timeout = "2h"
then = "@done"

[tasks.sync]
every = "1h"

[tasks.sync.command]
shell = "scripts/sync.sh"
timeout = "5m"
then = "@done"
```

Differs from shipped: today the compiler builds a turn step `agent` and a check `check` from row flags at each fire, filling the run timeout per fire. Here the row carries the node table itself, with `lifecycle` and `timeout` written when the row is added (R45).

## What replaces each compiler shape

Today's compiled shapes are built by `crates/rimz/src/harness/schedule/compile.rs::Graph`.

| Today's row | Compiled today | Target |
| --- | --- | --- |
| `--agent` | turn step `agent` | an inline `[tasks.<t>.agent]` with `lifecycle = "turn"` (example 7) |
| `--agent --stay` | stay step, `share` unless `--takeover` | an inline agent with `lifecycle = "session"` and the occupancy the flags name |
| `--agent --verify` | until step with check `verify` | a workflow (example 6) |
| `--check` (a shell guard) | check `check` routed by polarity | a workflow with a `test` node (example 4) |
| `--check` alone | check with `pass` to `@done`, `fail` to `@failed` | an inline `[tasks.<t>.command]` (example 7) |
| `--judge` | checks `judge-<tier>` that run the agent anyway on failure | a workflow with a `judge` node (example 5) |
| a decline with a recheck | sleep `recheck-<guard>` or `hold-<guard>` | a `sleep` node (example 5) |
| a decline held on `when` | condition `held-<guard>` and `rearm` | a `condition` node routing `@wait`, or a `sleep` with `when` and `for` |
| `--wait` (delivery) | no run | unchanged: the row's own action |
| `--wait` with a guard | check, then message step `deliver` pinned to the session | gone with the guards |

## Build map

Each link and the rules it owns:

- Schema (`language-schema`): R1 to R9, R11, R12, the field table's shape (the `admission` table and the three-way `occupancy` as fields), and, proposed here, the removal of `max-visits-per-outcome` and `reset-on` with their engine code (R40), since no definition can set them once the schema lands.
- Failures (`language-failures`): R10, R13 to R20, the reserved words of R7, and `crates/rimz/src/harness/workflow/engine.rs::resume` for this page: R20, the generated handler's branch, and the two count resets after a cap (the note under R40 and its premise).
- Lifecycles (`language-lifecycles`): R21 to R25, the names `turn` and `session` wherever they are written (the loop code on trunk included), and `on.asked` on a session.
- Timeout (`language-timeout`): R26 and R27.
- Conditions (`language-conditions`): R28 to R34, and R6's required `on.unknown`.
- Capacity (`language-capacity`): R35 to R39, `--give-up` and `--skip-capacity` on `rimz workflow run`, and a run of any origin accepting a give-up instant.
- Loop (`loop-triggers`): R41 to R49, with the compiler and `TaskFire::supersede_held_decline` removed.
- The compatibility removal and the typed pipeline need nothing from this page; the compatibility removal owns the one refusal R11's version rule reuses.

Meeting points, each stated whole:

- An expired capacity wait as a failure cause: capacity owns when `capacity` is raised (R37) and failures owns where it goes (R13); the expiry never passes the function that settles a finished step, so the chain needs one entry that both paths reach, which failures builds and capacity calls.
- A node timeout as a failure cause: timeout owns when the clock passes (R26, R27) and failures owns the cause's route; until the timeout link lands, the lifecycles and failures links keep today's clocks.
- The session's clock against its rest check's clock: timeout owns it (R27), and lifecycles builds the rest (R23, R24) on today's clock behaviour and changes no clock.
- The lifecycle an inline loop node names: lifecycles owns the two names and renames them wherever trunk writes them, loop owns the constructor that requires `--lifecycle` (R45), and whichever merges second adopts the other's change.
- The give-up instant a loop passes to a run: loop computes and passes it for every fire (R47), and capacity owns the argument on the run and what the engine does with it (R37, R44).
- A dead session at rest, closing and timeout together: failures owns the definition of dead (R15), lifecycles owns what a rest is, and timeout changes neither.
- `resume --at` on a live run (R20): failures.
- An optional `prompt-file` on an agent node and the refusal of an empty prompt (R46): loop, since the inline node needs it first.

## Decisions for the owner to confirm

- The eight kinds and their names (R1, R2).
- Route syntax (R4, R5).
- The cause list and the eight changes against shipped behaviour (R14, the list under Failures).
- The failure input (R17).
- The handler as an ordinary node, losing the generated handler's default prompt, its deadline exemption and its never being stopped (R13, R20).
- The definition of a dead session, a premise no build has run (R15).
- `on.asked` on both lifecycles (R16).
- `rest-check` as a list, and the rest-route rule (R23).
- The closing `delay` dropped, since `keep = true` leaves the agent open, losing a turn taken inside the delay reopening the judgement (R25).
- The team rule (R25).
- Both clocks running at a rest, the clock starting at admission, and a judge's clock pausing (R26, R27).
- `timeout` required on five kinds (R27).
- `none`, the refusal of `=unknown`, no veto, a hold surviving unknown, and the reader table (R28, R29, R31, R33).
- `else` and `on.unknown` required on a condition (R6, the field table).
- The admission fields, `give-up`, `--skip-capacity`, a give-up instant for every loop-started run, and `rimz workflow run` waiting (R35 to R38, R47).
- Sealing whole with a language version (R11).
- The row, the constructor with a required `--lifecycle`, eject as a move, the prompt as a start argument, and a guarded delivery lost (R41 to R48).
- `failed` reserved (R7).
- The validation of `visits` and `capacity` routes (R10).
- The warning for a cycle that never waits dropped (R9).
- The project-scope refusals (R12, R49).
- The visit-rule removal assigned to the schema link (R40, the build map).
- What an entry is, with `restart` keeping its reset, `--extend visits=+N` keeping its grant, and the reset on resuming after a cap gone with the park and the handler (R40 and the note under it).
- A row's `prompt-file` beside an inline agent's own `prompt-file`, one table apart with different meanings: an open naming choice (R44, the field table).
- The build map.
- What becomes of this page once the last link merges; recommended: delete it, since the guide and the reference then own every fact.

## Rules that still carry an exception

Each with what would remove it:

1. `timeout` is a field of six kinds, not eight (R27). Removed by giving message and sleep a timeout that means the same thing.
2. `timeout` is required on five kinds and optional on agent (R27). Removed by requiring it everywhere with an explicit `"none"`, or making it optional everywhere.
3. `rest-check` and `subscribe` exist only on a session (R23, R25). Removed by two kinds, `[turn.<n>]` and `[session.<n>]`, in place of `lifecycle`.
4. A team is a session with no rest check (R25). Removed by defining a team's rest.
5. `max-visits` exists only on work nodes (R40). Removed by counting check entries too.
6. `@wait` is a target only from a check (R4). Removed by dropping `@wait` and writing a wait as a route to the check's own name. Not proposed: a route to oneself reads as an immediate rerun, and R9 would then need the same check-only wording to tell a waiting self-route from a spinning one.
7. Two built-in retries, a judge's and a launch's (R19). Removed by dropping them, or by one `retries` field.
8. `give-up` and `--skip-capacity` cover the capacity wait only; the window and checkout waits are bounded by the deadline alone (R37, R38). Removed by one bound and one flag over the whole wait in front of a node.
9. The deadline fires once per grant (R18). Removed by making the deadline end the run with no route. Not proposed: that loses the handler that reports on a late run (example 3 reaches `reflect` on `deadline` through the workflow's `on.failed`), and firing again at every later node instead would fail that handler too.
10. A delivery starts no run (R42). Removed by an inline `[tasks.<t>.message]` node.
11. The cache keep-alive reads unknown as yes (R33). Removed by keeping warm only on true, like every other reader. Not proposed: an unread reading would then let a warm cache lapse, and the shipped code keeps warm on purpose (`crates/rimz/src/harness/cache_keepalive.rs::SeatPolicy::horizon` gives up its horizon only when the clause is false with every reading present).
12. A condition waits during a `for` hold without a route saying so (R32). Removed by a routable `holding` outcome.
13. `visits` and `capacity` are the two causes whose route is restricted (R10). For `capacity`, removed by counting a visit at entry, before the capacity wait, so the node's own cap ends the loop; not proposed, since a held start would then spend the node's visits doing nothing. For `visits`, no removal: once the cap is spent every further entry raises `visits` again, counted or not, so a route that reaches the node again never ends, and the restriction is what makes a cap a bound.
