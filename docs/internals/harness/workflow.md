# Workflow engine

A workflow is a durable run through a flat graph of nodes, defined in `workflow.toml` and read live: a deployed run reads its deployed directory at every engine read, and a loop task's inline run reads the definition its fire generated. Checks decide where the run goes next: a test or script command, a condition over loop facts, a sleep, or a judge (an agent in a pane that answers through `rimz workflow submit`). Steps act: an agent step launches a layout with one supervised leader, a command step runs a command, a message step queues a message for a living agent, and a runs step starts other workflows and joins them. A loop task can start runs, and the scheduler tick drives every active run so its clock bounds fire on time.

Three rules hold the engine together. The run record is the only truth: every transition reads it again under the room's workspace lock and publishes through one function, `engine::persist`. The journal beside the record is evidence. No route reads it, though the engine reads it to classify a terminal outcome, to build an agent's brief, and to tell whether a late message result was already delivered. A wake carries no state: it only spawns an `advance`, which reads the record. Record writes that are not transitions skip `persist`, and each is named where it happens: a hosted child's identity ([Hosted children](#hosted-children)), the loop-report mark and a task rename ([Loop tasks](#loop-tasks)), and the stranded-run ending ([The workflow pass](#the-workflow-pass)). Start reading the code at `crates/rimz/src/harness/workflow/engine.rs::advance`.

The [workflows guide](../../guide/workflows.md) teaches the format, and the [workflow CLI reference](../../reference/cli/workflow.md) owns every field rule, command, printed line, and JSON shape. This page owns how the engine decides, what each decision must keep true, and why.

## Where the code lives

Paths are under `crates/rimz/src/`.

| Path | Owns |
| --- | --- |
| `harness/workflow/spec.rs` | `load` and `parse`, the lowered model (`WorkflowSpec`, `Check`, `Step`, `Case`, `Wake`) with its table word (`CheckTable`, `StepTable`), the failure causes (`Cause`), the reserved words, validation `Issue`s, the visit bound, `run` argv resolution, window parsing, and the workflow time (`When`: `parse_when` checks a spelling without a clock, `When::resolve` turns it into an instant from a `now` and the `timezone` setting, its calendar forms through `crates/rimz/src/utils/time.rs::CalendarAt`, which loop and wait one-shot times share). The lowered model is private to `harness::workflow`: `parse` compiles it into a `Definition`, and only `parse`, `definition.rs`, `graph.rs` (through `Definition::authored`), admission validation, and the schema read it. The module is in the sidebar import graph. |
| `harness/workflow/spec/kinds.rs` | The authored file (`WorkflowFile`): the header and one `deny_unknown_fields` table per node kind, with one `*Edges` struct per kind for its `on` table. `rimz workflow schema` derives from these types. Also the one-name-across-kinds check and the lowering into `Check` and `Step`. In the sidebar import graph. |
| `harness/workflow/definition.rs` | The typed `Definition` that `parse` and `load` return: its `Node`s with their `Kind`, `Target`s, edges resolved once at compile, and every parsed value held beside its authored text (`Parsed`). In the sidebar import graph. |
| `harness/workflow/graph.rs` | The edge compile, the failure chain (`Definition::failure_route`), how a step's ending maps to a cause (`Definition::ending_cause`), fireability and mark text, routing (`Definition::target`), and the graph walk (`Definition::walk`). |
| `harness/workflow/validate.rs` | `validate` and `validate_spec`: required-field, value, cross-field, graph, and environment checks after the strict load, and the `rimz workflow validate` rows. |
| `harness/workflow/engine.rs` | Run control: `create`, `advance` with its locked `prepare`, `cancel`, `resume`, `extend`, `restart`, `wake`, `wake_run`, `reassign_task` (a removed task's release of its live runs), and `persist`. Also the run outcome a terminal write carries (`engine::failure`). |
| `harness/workflow/engine/agent.rs` | The agent-attempt protocol: `compose`, `launch`, `prepare_end`, which admits the off-lock stop, and `finish`, which settles it. |
| `harness/workflow/engine/rest.rs` | A session's rest-check moves: holding a session aside at its turn end (`begin`), the held preflight (`prepare`), restoring it, pushing back (`push`), and interrupting it. |
| `harness/workflow/engine/accounting.rs` | `transition` and `step`: the assist and diagnostic records for each persisted transition. |
| `harness/workflow/state.rs` | `Transition`: locked routing, the one failure entry (`Transition::failure`), waits and holds, bounds, agent-end staging, step settlement, non-agent end recording (`finish_script`), and the hosted-child kill. |
| `harness/workflow/script.rs` | `evaluate` and `execute_step`: host checks and command steps behind one interruptible child-publication boundary (`with_child`). Dispatches to `script/condition.rs`; a judge is an agent attempt, not a hosted child ([Judges](#judges)). `condition_now` is the read-only condition preview for inspection. |
| `harness/workflow/observe.rs` | The attempt-observation rules the host calls. `observe` projects the leader's run record and a room snapshot into `Observed`: the blocking ask, the stop that wins, the limit, the children. `stop_cohort` and `detach_cohort` decide which agents a stop or detach reaches, and `orphan` finds the unpublished supervised run of a dead launcher. The host applies each effect. |
| `harness/workflow/inspect.rs` | Read-only journal hops (`hops`, `reached`) and the timeout instants (`timeout_at`, `session_timeout_at`) shared by inspection and transitions. |
| `harness/workflow/brief.rs` | The `### Workflow` launch-reminder section for a step leader (`step`), the push-back message (`push`), and the shorter judge section (`judge`). |
| `harness/workflow/mod.rs` | The `AttemptHost` seam, `ScopeRoots`, `cancel_args`, and `advance_args`, the one `workflow advance` argv that run creation and both wake edges spawn. |
| `store/workflow.rs` | `RunRecord`, `JournalEntry`, their codecs, `VERSION`, `classify`, `list`, `journal`, `finish`, `seal_definition` (an inline run's generated definition), and `RunRecord::definition_dir`, plus the `RunAddress` derived from the recorded scope. Its one state change is `end_stranded`, which ends a run this RimZ cannot continue. |
| `cli/workflow.rs`, `cli/workflow/` | Parsing and presentation, without creating rooms: list, show and its rail, history and park views, definition and node views, receipts, and the hidden `reconcile` pass. `cli/workflow/host.rs` builds the production host with the terminal interaction. |
| `launcher/workflow_host.rs` | The production `Host`, which implements `AttemptHost` over the launcher, the room snapshot, and the tree stop, asking through the `Interaction` its caller passes. |
| `cli/workflow_view.rs`, `cli/workflow_view/` | Run phrases shared by workflow and loop inspection (`detail`, `glyph_role`), deployed-definition lookup, run selection (`crates/rimz/src/cli/workflow_view/resolve.rs::select`), and history rendering with the atomic `history.txt` copies (`history.rs`). |
| `crates/rimz/src/harness/schedule/signal.rs::wake_workflows` | The signal edge: reads records and spawns the wake helper. |
| `crates/rimz/src/harness/schedule/fire.rs::tick_workflows` | The clock edge: paces and spawns one `advance` per due run. |
| `crates/rimz/src/harness/schedule/runner/shell.rs::Fire::prepare_shell` | The loop fire that starts a run of a command task's one-check graph. |
| `crates/rimz/src/store/gc/classes.rs::collect_owned` | Retention of finished run homes. |

## Terms

| Term | Meaning |
| --- | --- |
| node | A check or a step. The run sits at one node at a time (`RunRecord.node`), entered at `entered_at`. |
| attempt | One evaluation of a check or one launch of a step, published on the record as a `LiveAttempt` of kind `script`, `judge`, `agent`, or `workflow` (a runs step, carrying its reserved `runs`). Test, script, condition, and sleep checks, command steps, and message steps all use `script`. A message step's attempt also carries the `message` id it queues. |
| seq | The attempt's number, allocated from `RunRecord.next_seq`. A settlement whose seq is no longer the live attempt's writes nothing. |
| host | The process that owns a live attempt (`LiveAttempt.host`: PID and process-start token). For a hosted check or command step it is the `advance` running the child. For an agent step it is the process that launches or stops the layout. |
| child | A hosted check's or command step's process group (`LiveAttempt.child`: leader PID and start token). |
| wait | The run stays at its node: a check routed to `@wait`, or an agent step whose admission is blocked. Repeats merge into `RunRecord.wait`. |
| hold | A wait that a condition case with `for` opens. The engine writes `hold` as that wait's outcome, and no check may answer it. Three other mechanisms share the word, and this page always names them in full: the ask hold (`held_since`, [The ask hold](#the-ask-hold)), the held session (below), and a capacity hold (`Wait.hold`, [Admission](#admission)). |
| held session | A session agent attempt set aside in `RunRecord.held` while one of its rest checks evaluates its turn end ([Rest checks](#rest-checks-judging-an-open-session)). |
| bound | The run deadline, a node's `timeout`, or a step's `max-visits`. Each raises a failure at the node. |
| failure | A cause raised at a node, routed through the failure chain with the failure input ([Failure causes](#failure-causes)). |
| fault | An evaluation error that is not a failure of the node: the run ends failed with no route (`Transition::fault`). |
| park | State `parked`. Only a route to `@user` (`Target::User` in `Transition::route`) sets it, and `parse` refuses `@user` as an authored target, so no run of a current definition parks. The parked branches of `resume`, `restart`, `list`, the tick, and the sidebar are unreachable today. This page still says what each does, so a change that removes `Target::User` knows what goes with it. |
| terminal | `done`, `failed`, or `canceled`. A terminal record never changes state again. |

## Definitions

Deployed definitions live at `<agents home>/workflows/<name>/workflow.toml` and in each installed pack's `workflows/<name>/workflow.toml`. `crates/rimz/src/harness/workflow/deployed.rs::Deployed` is the one name lookup: the home claims a name first, then the packs through `packs::resolve_claims`, so exclusions drop a pack copy and two pack copies conflict. Every reader of a name goes through it: `run`, `show`, `validate`, loop preflight, runs steps and their validate advisory, and the pack inventory, which lists each pack workflow as an item of type `workflows`. A name lookup reads no `workflow.toml`. Only the inventory does (`crates/rimz/src/harness/workflow/deployed.rs::Deployed::into_pack_items`): it runs each pack workflow that holds its name through `spec::load` and marks one the loader refuses as failed, with a one-line message that drops the parser's source snippet and can still name a refused key or value. A pack copy's path is spelled through the stable entry link `packs/<entry>/`, never through the commit checkout behind it. A path that is not a bare name is a directory anywhere. A loop task's fire builds an inline definition from its row instead ([Loop tasks](#loop-tasks)). The committed fixture `crates/rimz/tests/fixtures/workflows/babysit-pr/` shows script checks with declared wakes, supervised steps, a judge, and signs. Nothing installs it.

### Loading and compiling

A definition is strict: an unknown field, a field the node's kind does not take, and a wrong-typed value all refuse at their dotted path when the file loads. A value of the right type that breaks a rule (a zero `max-visits`) loads, and `validate_spec` refuses it below. `rimz workflow schema` derives its JSON Schema from the loader's types (`crates/rimz/src/harness/workflow/spec.rs::schema` over `WorkflowFile`), so `additionalProperties: false` in the schema and the loader's refusal are one rule.

`crates/rimz/src/harness/workflow/spec.rs::load` reads the text and calls `crates/rimz/src/harness/workflow/spec.rs::parse`, the same entry `engine::create` and every engine read use. `parse` runs four stages:

1. `reject_reserved_targets` refuses any `@user` or `@break` target an author wrote.
2. `crates/rimz/src/harness/workflow/spec/kinds.rs::deserialize` reads the `toml::Value` into `WorkflowFile` through `serde_path_to_error`. Every table carries `deny_unknown_fields`, so the first error refuses alone, as `<dotted path>: <serde message>`. An unknown key is named at the key, a missing required field at the field, and a syntax error keeps its line, column, and excerpt.
3. `WorkflowFile::lower` refuses a name used by two kinds (`SpecErr::DuplicateNode`), then lowers each kind table into a `Check` or a `Step` that records its table word. Validation paths and the `validate` KIND column print that word.
4. The lowered spec compiles into the `crates/rimz/src/harness/workflow/definition.rs::Definition` that every reader consumes: the engine, the graph, inspection, scripts, the brief, throttle limits, the CLI views, the sidebar seeds, and schedule probe demand.

Compiling never fails. It parses each node's kind, route targets and edges, conditions, durations, windows, budgets, stagger, wake selectors, subscriptions, and the `shlex` split of `run` once, and keeps each beside its authored text, error included. So a view prints what the author wrote. Every `advance` validates the definition again ([Engine reads](#engine-reads)), so a value validation refuses makes a live run wait instead of reaching its use site there. `resume`, `extend`, `restart`, and the signal helper admit it the same way before they change the run ([Engine reads](#engine-reads)). A node with no single runner has no `Kind`, and a route to a name that is no node is still a node target. Values that read the clock, the filesystem, or live configuration stay at their use: the header deadline's timestamp, slash-word resolution of `run`, prompt and system-prompt files, judge profiles, and launch groups.

`validate_spec` then applies the required-field, value, and graph rules: an invalid optional value (a zero `max-visits`, an overlong duration, `mode = "plan"`, an unparsable `budget`), a route for an outcome the check cannot answer, and each cross-field rule refuse with the path and the fix. It warns without refusing only on what the definition alone cannot settle:

- Stage names checked outside a workspace.
- A turn step, or a session with a `rest-check`, whose leader adapter cannot report background work (`do_background`).
- Machine auto-continue off, one warning for each agent step: one paused on a provider limit waits for the user. A judge takes none, since its timeout runs through a limit ([Judges](#judges)).

`rimz workflow validate [dir] [--json]` loads agent profiles through the machine definition catalog and never creates a room. Its argument is a directory anywhere, a deployed workflow name, or a command loop task, whose one-check graph it writes to a scratch directory (`crates/rimz/src/cli/workflow/definition.rs::Definition::resolve`). A name that is both a deployed workflow and a task refuses until it is written `workflow:<name>` or `task:<name>`. Without an argument it validates every name `Deployed::every` lists, a pack conflict as an error. `rimz workflow run` (`crates/rimz/src/cli/workflow.rs::start`) runs a directory anywhere, except that a loop task's start and a start whose starter chain roots at an agent (`crates/rimz/src/cli/workflow.rs::agent_started`) refuse a directory `Deployed::claim` does not name: a deployed workflow's directory, links followed, or one inside the home's workflows directory. That walk reads each starting run's record and counts an unreadable one as an agent's, so the refusal fails closed. A caller with `RIMZ_WORKFLOW_NODE` set, no live run in `RIMZ_WORKFLOW_RUN`, and a resolved agent starts with no starter (`crates/rimz/src/harness/workflow/engine.rs::starting`), so a kept agent's variables in a hosted child owe that agent nothing, but `crates/rimz/src/cli/workflow.rs::starting_here` still reports it for the refusal. Nested runs steps take names only, so they never reach a loose directory.

Workflow `timeout`, `sleep`, `for`, and workflow and step `stagger` use `harness::duration::duration`: `s`, `m`, `h`, or `d`, at most `365d` inclusive. An overlong value refuses validation, at the start and at every `advance` of a live run, so a run whose definition carries one waits until it is fixed ([Engine reads](#engine-reads)). `admission.give-up` keeps the separate `config::CapacityWait` parser and is unbounded.

### The header

The [reference](../../reference/cli/workflow.md#definition-fields) owns the header fields and their rules. Two facts belong to the engine. The run record stores the instant `run --deadline`, else the header `deadline`, resolved to at start (`RunRecord.deadline` in `crates/rimz/src/store/workflow.rs`), and `extend --deadline` replaces it with the instant its own time resolves to at the extend. An agent step's own `worktree` setting moves that step out of the run's `scope` checkout ([Agent steps](#agent-steps)).

### Kinds

Each node is written as one of nine tables, and the table word is its kind, so no field selects a runner. `WorkflowFile::lower` turns `test`, `script`, `judge`, `condition`, and `sleep` into a `Check`, and `agent`, `command`, `message`, and `runs` into a `Step`. `Check::kind` and `Step::kind` read the lowered fields and compile into `Node::kind`. The [reference](../../reference/cli/workflow.md#definition-fields) lists every field each table takes and its rules. The lowering:

| Table | Lowers to |
| --- | --- |
| `test` | `Check`, `CheckKind::Test`, holding `Invoke::Run` or `Invoke::Shell` by `run` or `shell` |
| `script` | `Check`, run or shell |
| `judge` | `Check`, `CheckKind::Judge` |
| `condition` | `Check`, `CheckKind::Condition` |
| `sleep` | `Check`, `CheckKind::Sleep` |
| `agent` | `Step`, `StepKind::Agent` ([Agent steps](#agent-steps)) |
| `command` | `Step`, run or shell |
| `message` | `Step`, `StepKind::Message` ([Command and message steps](#command-and-message-steps)) |
| `runs` | `Step`, `StepKind::Runs` ([Runs steps](#runs-steps)) |

A condition's `cases` are required by its type (`crates/rimz/src/harness/workflow/spec/kinds.rs::ConditionNode`), `else` and `on.unknown` by `crates/rimz/src/harness/workflow/validate.rs::Validation::condition_fields`, and its `timeout` by `Validation::requires_timeout`. A condition takes no `wake`, since its subscriptions come from its terms. Every `advance` validates again, so a judge without a `summary` makes its run wait, and the launch note never describes a judge without one.

A condition's cases may be written inline or as `[[condition.<name>.cases]]` blocks, and the loader reads both into the same ordered array. A case's `when` terms may not match the value `unknown`: the shared `WhenExpr` parser refuses it in every condition reader, since a term over an unread key is already unknown by itself.

A `wake` entry goes through the loop's selector and match validator. An agent-family selector and a team selector other than exact `team.stage` refuse at the entry's index, with `SpecErr::LifecycleWake` supplying the reason. [Subscriptions](#subscriptions) owns what `subscribe` may name.

### Agent steps

`agent` takes the fresh-launch layout grammar ([reference](../../reference/cli/workflow.md#definition-fields)). A team runs only in a session without a `rest-check`, since a team has no single turn end or rest to judge: `crates/rimz/src/harness/workflow/validate.rs::step_layout` refuses it on a turn or a rest-checked session, naming `lifecycle = "session"` and the removal of `rest-check`. The host launches a team through the launcher's team layout path with the step's leader run.

The engine rules behind the agent-only fields:

- `crates/rimz/src/harness/workflow/spec/kinds.rs::AgentNode` gives `lifecycle` no default, so a missing or other value is a parse error. `Step::lifecycle` reads the written value, and command steps stay turn-shaped.
- `rest-check` refuses a sleep because a rest check samples leader turn ends, and a sleep runs its own timer.
- A step's `timeout` runs from its launch (`crates/rimz/src/harness/workflow/inspect.rs::timeout_at`).
- `on.asked` and `on.errored` may not target `@wait`, and validation refuses either with `only checks can wait`.
- `admission.max-parallel` and `admission.stagger` lower into the step's limit, the same shape as `workflow.limit` ([Admission](#admission)).

`crates/rimz/src/harness/workflow/definition.rs::Node::launch_overrides` resolves the launch settings, `engine::agent::launch` passes them in `StepLaunch.overrides`, and the host maps them onto the launcher's overrides and cohort request as a fresh launch applies them (`crates/rimz/src/launcher/workflow_host.rs::Host::launch`). A `--continue` restart resumes the provider session, and the host drops `account`, `system-prompt-file`, and `worktree` there. The relaunch takes the room's current login for the kind when it shares a history pool with the session's (`crates/rimz/src/harness/resume.rs::relaunch_login`), and otherwise refuses. It runs in the run's scope checkout. `account` also pins every cell's start group for capacity admission, which then skips availability routing. The engine asks the host for those groups (`AttemptHost::start_groups`), and both hosts resolve them through `harness::workflow::launch_groups`.

`max-visits` must be at least 1. It caps the step's entries counted in `RunRecord.visits`, a plain count per step (`crates/rimz/src/harness/workflow/spec.rs::Step::tripped_visit_bound`), increased by the step's own grant in `RunRecord.visit_grants`. Every step kind counts at admission, before it acts. Only a manual restart clears a count.

### Command and message steps

A command step's `run` resolves the same way as a test's or a script's ([`run` and `shell`](#run-and-shell)), and its `shell` text must be nonempty.

A message step's `then` names one node, `@done`, or `@failed` (`crates/rimz/src/harness/workflow/validate.rs::message_then`). Its `message` must be an `@` address without a pane id that `address::parse_selector` accepts and that is neither `@user` nor `@me` (`crates/rimz/src/harness/workflow/validate.rs::message_target`). Its optional `session` pins the agent by launch: the session given, or the session a restart or molt of that launch replaced it with (`crates/rimz/src/launcher/workflow_host.rs::pin_names`). A handle that another launch took over goes undelivered. `prompt-file` is optional and confined. `agent::compose` joins the prompt file when set, the node's sign, the run prompt, and the fire's evidence, and `engine::Notice::text` writes a header line before them. [Message steps](#message-steps) owns how one runs.

### `run` and `shell`

`run` is an argv parsed with `shlex`, never shell text: pipes, variable expansion, and redirection belong inside a script. Every word containing `/` names a file relative to the workflow directory (`crates/rimz/src/harness/workflow/spec.rs::resolve_argv`). Only normal path components are accepted, and canonical resolution must stay inside the directory. Other words pass through unchanged (`crates/rimz/src/harness/workflow/spec.rs::resolve_words`): a program such as `sh` or `uv` is found on `PATH` when the process spawns, and any other word is a plain argument. Files are resolved and hashed again at every evaluation.

`shell` is unrestricted `sh -c` text, and the paths it references are neither confined nor hashed.

Checks must be idempotent. A host killed after a script's effects but before settlement causes a fresh evaluation, never recovery from the previous stdout.

### Edges and the graph

An edge names another node, `@wait`, `@done`, or `@failed`. Checks and steps share one name namespace, and an author-written node or workflow name is nonempty ASCII letters, digits, `-`, and `_` (`crates/rimz/src/harness/name.rs::validate_name`). Every `on` value and every `then` is a target string.

`crates/rimz/src/harness/workflow/graph.rs::Definition::compile_edges` emits a node's answer routes, then one edge per cause its kind raises, each resolved through the failure chain. A session's rest checks compile to edges with outcome `rest-check` (`graph::REST_CHECK`), appended after the step's other edges, so the walk, the cycle bound, and the unreachable list see each listed check. Engine routing, validation rows, timeout admission, cycle detection, and inspection all read these edges, so a rule about where a run can go is a rule about `compile_edges`.

Each edge also carries fireability and, when it can never fire, a typed reason (`crates/rimz/src/harness/workflow/graph.rs::Definition::fireability`). `Edge::mark` owns the wording validation and inspection print.

- A `timeout` edge never fires on an agent step without a `timeout`, the one kind where the field is optional.
- A `visits` edge never fires without `max-visits`.
- An agent step's `capacity` edge always fires: a start wider than a machine cap raises it on any run, a `--skip-capacity` run included, so the return check (step 7 of [Adding a failure cause](#adding-a-failure-cause)) and inspection follow it whatever the step's `give-up`.
- An unset header deadline makes a `deadline` edge conditional on a deadline the run gets from `workflow run --deadline` or a later `extend --deadline <when>` (`Fireability::WithDeadline`), not never-firing. The mark reads the definition alone, so `workflow validate` and the definition views print it; a run's own views print no `deadline` edge.

`crates/rimz/src/harness/workflow/graph.rs::Definition::walk` takes reverse postorder from `start`, traversing fireable edges last to first in display order, and lists unreachable nodes alphabetically. A step hangs under its `then` check only when that check occurs earlier in the walk and has a fireable edge to the step. This keeps a forward return, such as a `triage` check routing back to a `ruled` step, standalone. Hanging children follow their parent's edge order.

`crates/rimz/src/harness/workflow/script/condition.rs::case_marks` projects the latest answer's readings through `WhenExpr::truth`, stopping at the winner or the first unread case, so later cases are not marked. A holding wait's `case` selects the held winner. The adjacent `unread_reasons` recovers per-term unread reasons from the answer's stored prose, using its null-reading keys as delimiters so commas and parentheses inside a reason survive. It adds no stored field.

### Adding a step kind

A new step kind is read against every match that groups the acting kinds, which `rg -n 'Kind::Agent\(_\) \| Kind::Message' crates/rimz/src` lists. Its key spellings come from the kind tables in `crates/rimz/src/harness/workflow/spec/kinds.rs`, which rename fields to kebab case (`prompt-file`, not `prompt`).

A step that creates worktrees is read against every other creator (`rg -n -l 'emit_worktree_created|require_readable_core' crates/rimz/src`) for the readable-core check and the `worktree.created` signal. A step that works off the commit lock names its reread under the lock, and the test that holds it in the gap, before it is built. Its proofs include the human `workflow validate` table and the history detail line, which the `--json` tests bypass.

### Failure causes

A failure is one of eight causes, raised at a node. `crates/rimz/src/harness/workflow/spec.rs::Cause` is the closed set, and `Cause::raised_by` is the one table of which kind raises which. Every other reader (edge compile, validation, the schema test) derives from it.

| Cause | Means | Raised by |
| --- | --- | --- |
| `died` | The node's process could not start or ended abnormally. A step attempt that ends `died` or `canceled` raises it. | agent, command, runs, test, script, judge |
| `invalid` | An answer the node cannot use. | script, judge |
| `timeout` | The node's clock passed ([Bounds](#bounds)). | agent, command, runs, test, script, judge, condition |
| `errored` | A judge's own turn ended on a provider error that waiting will not clear. A step's errored turn is an attempt outcome routed by the step's own `on.errored`, never this cause. | judge |
| `undelivered` | A message found no single living agent. A message attempt that ends `died` or `canceled` raises it. | message |
| `visits` | The entry would pass `max-visits`. Raised before the node acts, counting no visit. | agent, command, message, runs |
| `capacity` | The start gave up waiting for capacity, or can never fit. Raised before the node acts, counting no visit. | agent |
| `deadline` | The run's deadline passed. Fires once per grant, at the node the run is at. | every kind |

Every failure goes through one entry, `crates/rimz/src/harness/workflow/state.rs::Transition::failure`. It builds the failure input `{outcome: "failed", cause, node, reason, detail}` plus the inherited `key`, journals a `Route` under the cause's word, and takes the target `Definition::failure_route` resolves: the node's `on.<cause>`, else its `on.failed`, else `[workflow] on.failed`, else `@failed`. The resolved source is `EdgeSource::Explicit`, `NodeFailed`, `WorkflowFailed`, or `Default` (wire `on.failed`, `workflow.on.failed`). A failure route is never `@wait` (`Validation::failure_target`): the failure has ended the node's work, and waiting there would hold a run that nothing rechecks.

`detail` holds the cause's own facts. A step ending builds it from the attempt's evidence (`code` and `log`, when present). `visits` adds `{visits, max}`, `capacity` adds `{waited_ms, first}` (and `never_fits` for a start too wide for a machine cap), and `deadline` adds the instant.

A step attempt's ending reaches a cause through `Definition::ending_cause` under `Definition::target`: `died` or `canceled` raises `died` (`undelivered` for a message step), a step's `errored` raises nothing and takes the step's own `on.errored`, any other cause word raises itself, and only a clean end follows `then`. A check reaches a cause by its kind, as step 4 of [Adding a failure cause](#adding-a-failure-cause) lists.

A fault is different. A condition's or sleep's evaluation error is not a failure of the node: `Transition::fault` journals `{outcome: "fault", reason}` and ends the run failed with no route.

### Adding a failure cause

A new cause touches every place below. The compiler stops you at only some of them, so walk the list.

1. Add the variant to `Cause`, to `Cause::ALL`, and its word to `Cause::word`. The word becomes reserved through `crates/rimz/src/harness/workflow/spec.rs::reserved`, so no author outcome may use it from then on.
2. Add it to `Cause::raised_by` for each kind that raises it. Each list is in display order, so the position you choose is where the edge appears in `workflow validate` and inspection. Edge compile, the dead-route check in `validate_spec`, and the return check (`returning_failures`, step 7) all read this table.
3. For a step kind, a test, or a sleep, add an `Option<String>` field to that kind's `*Edges` struct in `crates/rimz/src/harness/workflow/spec/kinds.rs` (`AgentEdges`, `CommandEdges`, `MessageEdges`, `TestEdges`, `SleepEdges`; a runs step shares `CommandEdges`), and add an entry to the `routes([...])` list in that kind's arm of `WorkflowFile::lower`. Without the field, `deny_unknown_fields` refuses the author's `on.<cause>`. Without the lowering entry, the route parses and is dropped without a word. Script, judge, and condition nodes take their `on` table as a map, so they need no field. The test `every_kind_table_names_exactly_the_causes_its_kind_raises` in `crates/rimz/src/harness/workflow/tests/failures.rs` compares each `*Edges` schema table with `raised_by`, and fails until the two agree. The new field also changes `crates/rimz/src/harness/workflow/snapshots/rimz__harness__workflow__tests__schema.snap`: review the change and accept it with `cargo insta accept`. Tests pin the edge lists too: `crates/rimz/src/harness/workflow/tests/graph.rs` for the agent kind, and the `workflow validate --json` golden in `crates/rimz/tests/integration/workflow.rs` (`validate_json_shape_errors_and_exit_status`) for a test node.
4. Raise it through `Transition::failure` or `Transition::fail`, with a reason and a `detail` object that holds the cause's facts. Never build the failure input by hand. Where the cause comes from depends on the kind:
   - A script check sets its error answer's `Cause` on `Evaluation::failure` in `crates/rimz/src/harness/workflow/script.rs`, and `crates/rimz/src/harness/workflow/engine.rs::advance` passes it to `Transition::check_failure` typed. A judge's settlement passes its `Cause` the same way.
   - A test, condition, or sleep check leaves `Evaluation::failure` unset (`crates/rimz/src/harness/workflow/script.rs::evaluate_test` starts from `Evaluation::failed(None, ..)`), `advance` passes a cause only for a script, and the check's error goes to `crates/rimz/src/harness/workflow/state.rs::Transition::finish_check`, whose match picks the cause by kind alone (`test` raises `died`, a condition or sleep error is an engine fault, the other kinds `invalid`). A second test cause therefore needs two changes: `evaluate_test` sets the new cause where the condition arises, and the cause reaches `check_failure` (the script arm in `advance` extended to tests, or a cause passed into `finish_check`). An error path left unset still settles through `finish_check` as `died`.
   - A step attempt reaches a cause through `Definition::ending_cause`, which maps `died` and `canceled` and otherwise parses the outcome word, so an outcome whose word is the cause's word needs no case there. When the new cause's word is already an `AttemptOutcome` word that steps route by name (`asked`, `errored`), `Definition::target` would resolve that step outcome through the failure chain from then on, since it asks `ending_cause` before the step's own edges: give `ending_cause` a step case that returns `None` for it, as it has for `errored`, and test the step's own route. A new `crates/rimz/src/store/workflow.rs::AttemptOutcome` variant changes the record format (step 9). The compiler asks for its arms in accounting, rest checks, and the brief, but `crates/rimz/src/harness/workflow/inspect.rs::attempt_superseded` ends in `_ => false` and needs its arm by hand.
5. For a step, decide the default reason text in `Transition::settle_step` (its match on the cause). A check's reason is the error text its evaluator returns. For every kind, decide the `crates/rimz/src/store/workflow.rs::ParkReason` in `Transition::failure`'s match. Both fall back silently (`the attempt ended <outcome>`, `ParkReason::Invalid`). The park reason feeds only the unreachable park path ([Terms](#terms)), so `errored` takes the `Invalid` fallback.
6. When the cause can fire only with a field set, add its case to `Definition::fireability` with a `NeverReason`, so validation and inspection do not show an edge that never fires.
7. When a step's cause is raised before the node acts and counts no visit, add it to the `[Cause::Visits, Cause::Capacity]` list in `crates/rimz/src/harness/workflow/validate.rs::returning_failures`. Otherwise a route back to the node repeats the failure without end. A check counts no visits, so a check cause never goes there.
8. When a loop task should report the run differently from `failed`, extend `engine::failure`, which classifies a failed run's `RunEnding` from the journal, and the mapping from `RunEnding` to a loop result in `crates/rimz/src/harness/schedule/run_log.rs::report_row`.
9. A new reserved word refuses every definition that already uses it as an author outcome, and a live run of such a definition waits at its next engine read ([Engine reads](#engine-reads)). It raises no version. A new `AttemptOutcome` variant changes the record's shape, so it raises `store::workflow::VERSION` ([The record and its version](#the-record-and-its-version)). The record snapshot `crates/rimz/src/store/workflow/snapshots/rimz__store__workflow__tests__record.snap` and each "version 19" on this page and in the reference change with it. The doc comment on `store::workflow::VERSION` says what a reader of run homes does with a run of another version.
10. Document it: the cause table above, each "eight causes" on this page (the count moves with the set), in the reference, and in the guide, the guide's sample `workflow validate` output when the kind it shows raises the cause, the [failures section of the reference](../../reference/cli/workflow.md#failures), and the guide's [When something fails](../../guide/workflows.md#when-something-fails) when a user meets the cause.

Run the module's tests with `cargo xtask test 'harness::workflow'` and the store's with `cargo xtask test 'store::workflow'`. [gates.md](../../contributing/gates.md) owns the gate stack.

### Rules and why

Validation enforces each of these, and the engine the last. Each row says why the rule exists, so a change that removes one knows what it reopens.

| Rule | Why |
| --- | --- |
| The eight causes, `limited` (reserved so no script answers it, though no cause raises it), `failed`, `asked`, `unknown`, and `hold` are reserved (`crates/rimz/src/harness/workflow/spec.rs::reserved`). No case or `else` uses one, and no check answers one, except a condition's or conditional sleep's `unknown`. | The engine produces them for its own routes. A script that answers a reserved word has given an answer no outcome names, which is `invalid`. |
| A `command`, `test`, `script`, `judge`, or `condition` needs a `timeout` (`Validation::requires_timeout`). An agent's is optional, and `message` and `sleep` take none. | Every check is bounded, so no check holds a run until someone comes: a judge's timeout runs from its launch through any question, limit, or budget park. A sleep has its own finite duration and a message send does not wait. An agent step's is optional, and its clock pauses while its agent cannot work. |
| A cycle through steps needs a `max-visits` on one of them. Every route counts as an edge except a `deadline` route. | Otherwise the run can revisit its steps without end. |
| A check-only cycle needs a `@wait` edge. A hold is not an edge and does not count. | Otherwise one advance would loop without pause. |
| `@wait` is a target only from a check, and never from a failure route. | Writing a wait as a route to the check's own name was rejected: it reads as an immediate rerun, and the cycle rule would need the same check-only wording to tell a waiting self-route from a spinning one. |
| A resolved `visits` or `capacity` route cannot lead back to its node (`returning_failures`). | Neither failure counts a visit, so a route back repeats the same failure without end. Counting a visit at entry, before the capacity wait, would let the cap end the loop, but a held start would then spend visits doing nothing. |
| `max-visits` exists only on steps. | A check-only cycle is bounded by its `@wait` rule instead, so check entries are not counted. |
| `rest-check` and `subscribe` exist only on a session, and a team runs only in a session without a `rest-check`. | A turn ends at its first clean turn end, so there is nothing to recheck or subscribe for. A team has no single turn end or rest to judge. |
| One built-in retry exists: a step launch that started nothing runs once more. A judge's launch never evicts, so it never reports one, and a judge launch that fails dies at once. No `retries` field exists. | The retried attempt has not acted on the checkout: an unstarted launch launched no agent. |
| `give-up` and `--skip-capacity` cover the capacity wait only. | The window and occupancy waits have no bound of their own. A run that must not wait on them without end sets a `deadline`. |
| The deadline fires once per grant, at the node the run is at. | The handler it routes to (often through `[workflow] on.failed`) must be able to run. Firing again at every later node would fail that handler too. `extend --deadline <when>` re-arms it. |
| A condition waits during a `for` hold without a route saying so. | `hold` is reserved, so no check answers it. Only the engine writes it, as the wait's outcome, and the held case's own outcome routes once the hold has run its length. |
| A run reads its definition live, and every `advance` admits it again ([Engine reads](#engine-reads)). A definition admission refuses leaves the run waiting, and one the run no longer fits ends it failed. No language version exists, and the record version tracks only the record's shape. | An edit reaches in-flight runs at their next step, so a fix to a prompt or a route needs no restart. A language version would rise with every change to what a field means, and each raise strands every unfinished run ([The workflow pass](#the-workflow-pass)). Waiting on a refused definition keeps a half-finished edit from failing the runs that read it. |

## Starting a run

`engine::create` admits a run in this order:

1. Compare every nonterminal run against the candidate, unless the candidate is a try (`StartArgs::once`, from `run --once`). A run blocks when it is not a try, when its scope path (`RunRecord.scope.path`) is the path `ScopeRoots::path` gives for that run's own scope kind, and it shares the candidate's origin and name, or its task. A match is the typed `WorkflowErr::Exists`.
2. Read the prompt. An inline definition is written into the new run's `definition/` here (`seal_definition`, which creates the run's directory). A deployed run copies nothing. No record exists yet.
3. Read the definition bytes, load them through `spec::parse`, and validate the lowered spec with `validate_spec` against the run's project, as every later advance read, `resume`, `extend`, `restart`, and the signal wake do.
4. Require the room's workspace record when the graph has an agent step, a message step, or an agent check, or when a loop task starts the run (`WorkflowErr::NoRoom`). `rimz start` creates the room. The library needs the room only in those cases, but `rimz workflow run` opens the room before it calls `create`, so a run a person starts always needs one.
5. Place the start (`engine::publish`): the node `--at` names (`Definition::node`, reachability ignored; an unknown node refuses with `unknown workflow node <node>`) or the definition's start, the `--input` object as the node's `input` with its `key` as the run's key, and `RunRecord.once` for a try.
6. Under the workspace lock, repeat step 1, allocate the run's notes folder when it keeps one (`store::notes::allocate`), and write the run home (`store::workflow::initialize`). A start given `--at` or `--input` then journals a `Start` entry at the node, the journal's first entry, with the input when there was one. A failed write fails the start.
7. Spawn a detached `advance`.

The order is deliberate:

- Step 1 runs before the prompt or definition bytes are read, so a held run wins over a broken definition, a prompt-file error, or a scope-kind edit.
- Comparing each existing run by its own scope kind keeps a worktree run at the project root from blocking a different worktree.
- Matching by task means a task whose row changes what it runs still overlaps its own live run.
- A failure after step 2 removes the run's directory, an inline run's `definition/` included, since no record exists to keep it.

A try stops in `crates/rimz/src/harness/workflow/state.rs::Transition::route`, past the terminal return and the held-session branch: a route to `@wait` whose outcome is `unknown` waits as any run's does, and every other route, whatever its target (`@failed`, a failure or deadline route, a fault, a tripped visit bound), ends the run `done` with a `Terminal` entry naming that target in `tried`. The node is not entered and nothing is handed on, and `engine::outcome` names the ending `tried` from the record's `once` alone. Engine holds, a session's kept or pushed-back verdict, cancel (`canceled`), and a misfit (`failed`) never reach that branch. A live try collides with nothing: `create` skips step 1 for it and skips it as an existing run, and a workflow task's fire skips it as a hand-started run (`Fire::hand_started_run`).

`rimz workflow run` validates the definition directory before it calls `create`, so a manual start of a broken definition refuses on the definition even while another run holds the slot. The CLI prints `Exists` as a refusal. A loop fire that reaches `create` follows its task's `overlap` policy instead ([Loop tasks](#loop-tasks)).

`rimz workflow run` resolves `-w` before it calls `create`, after the read-only-home check, the cross-repository confirmation (`launcher::checkout::confirm_cross_repo_worktree`, which refuses without a terminal) and the launch name admission (`harness::lanes::admit_launch_worktree_name`), through `launcher::checkout::resolve_launch_checkout` with the configured base (`crates/rimz/src/cli/workflow.rs::named_checkout`); a tree it creates under a loop fire carries that fire's stamp. A missing RimZ-owned worktree is created as an agent launch's `-w` creates it, and an unmanaged linked worktree of that name is entered only on a terminal's confirmation (`CliInteraction::unmanaged_checkout`). Without `-w` the checkout is the current one. A worktree-scoped run runs in that checkout. A project-scoped run always runs at the root, so `-w` on a project-scope workflow refuses with `<name> has scope = project, so it runs at <project root>; start it without -w`, and a loop fire never passes it. Run ids are `wf_` identities.

`create` writes no transition, so `rimz workflow run` learns what happened from the record. Its receipt prints the run id, then waits up to 30 seconds for the first advance's decision and says when the run is held (`crates/rimz/src/cli/workflow/receipt.rs::first_pass`). A run that ended in that pass is waited for until its advance exits, since the advance's CLI moves the home after the engine returns (`crates/rimz/src/cli/workflow.rs::finish_run`); the receipt then names the finished home's work directory, and a restart's receipt says how the run ended from the record that pass last read in the live home, or, when the home moved before a read saw the run end, from the record in the finished home.

## Moving a live run

### Cancel

`cancel` takes the workspace lock and returns false on a terminal record, so the first terminal write wins. It kills the child of a hosted check or command step and ends the run `canceled`. For an agent step it stages a `canceled` end with cause `Cancel` and stops the attempt ([Stopping an attempt](#stopping-an-attempt)). While a live host is mid-launch or already ending the attempt, cancel leaves the stop to that host. Cancel needs only the record (`crates/rimz/src/harness/workflow/engine/fit.rs::close`): it uses the definition when it still parses and an empty one otherwise, so a run whose definition is missing, does not parse, or is refused still cancels, and its outcome's `acted` falls back to whether the run counted any visit.

An optional cancel reason travels through `Ending` to settlement and the `Cancel` journal row, including deferred agent stops. The hidden `workflow cancel --reason TEXT` flag carries internal cancellation causes, and `crates/rimz/src/harness/workflow/mod.rs::cancel_args` builds its argv with the machine-wide run id. The reason is omitted from both when none was given and default to absent on older records.

### Resume

`resume` (`crates/rimz/src/harness/workflow/engine.rs::resume`) refuses a terminal run, and an active run without `--at`. It grants nothing; `extend` does ([Extend](#extend)).

- A parked run re-enters its current node, or `--at <node>`, with a fresh `entered_at` (so a fresh timeout clock), keeping its input, key, and every visit count.
- `--at` on an active run moves it too (`move_live_attempt`). A live agent attempt stages a `canceled` end with `EndCause::Resume` and a saved route to the destination. A hosted child is killed and its attempt settled. The run journals `Resume` at the destination.
- `--at` on a step whose visits reach its `max-visits` plus its grant refuses with `WorkflowErr::SpentVisits` before anything changes, and the CLI prints the `rimz workflow extend <run> <step> --visits +1` command. Without the check the move would end the live attempt and then fail the step with `visits` at once.
- While an agent attempt at the current node is already ending and its host is live, `--at` refuses with `the attempt at <node> is settling; retry resume after it finishes`.
- `--at` on a try refuses before anything changes, with ``workflow run <id> is a try, which ends at its node and cannot be moved; cancel it with `rimz workflow cancel <id>` ``, since the move would route and so end it. `extend`, `restart`, and `cancel` work on a try.

A message attempt that resume settles after its host died has its queued notice retired through the caller's `AttemptHost::retire` once the record persists (`engine::orphaned_notice`), as cancel and the deadline do. A live host retires its own notice when it relocks and finds the attempt gone.

`resume` returns the record it persisted before it spawned the detached advance, so a receipt need not reload a record that the advance may already have changed.

### Extend

`extend` (`crates/rimz/src/harness/workflow/engine.rs::extend`) records a grant under the commit lock and nothing else: it enters no node, touches no attempt, and journals one `Extend` row (`step`, `visits`, `deadline`, each absent when not granted). A terminal run refuses. An active run spawns an advance after the persist, so a step that a grant admits again, or a deadline that a new one replaces, is read at once. A parked run keeps the grant for its resume.

- `--visits +N` adds N to `RunRecord.visit_grants` for one step (`RunRecord::visit_grant` reads it, 0 when absent). `granted_step` picks the named step, else the step of the run's held or live session (`RunRecord::session_attempt`), else the run's node, and refuses a node that is no step, a check, and a step without `max-visits`. Restarts and visit resets leave the grants as they are.
- `--deadline <when>` resolves the time at the extend, replaces `RunRecord.deadline`, and clears `deadline_fired`, so the deadline fires again.

No run parks on a spent cap or a fired deadline: a run parks only on a route to `@user`, which `crates/rimz/src/harness/workflow/spec.rs::reject_reserved_targets` refuses as an authored target, so each takes its failure route. A grant therefore reaches a run that has already failed a step only through a `resume --at` after it.

### Restart

`restart` stages `EndCause::Restart { continued }` on the current or restored held agent and reuses the off-lock stop and the sequence- and host-checked settle.

- At a parked agent step with no session attempt, a matching-node `prev_attempt.run_id` identifies the previous session.
- With `--continue`, restart restores that attempt for stopping, including a still-live detached card, before the replacement launches. `record.restart.settled` marks its already-journaled sequence, so settlement does not repeat its end or cost.
- Settlement keeps input and key, re-enters the owning step, clears its visit count, and journals `VisitsReset { step, outcome: "restart" }` and `Restart { step, continued }`.
- A durable `record.restart` carries the optional leader session through stop recovery and occupancy waits until launch publication consumes it. Parking, terminal settlement, a route to another node (the `Target::Node` arm of `Transition::route`), and a `resume --at` to another node clear it too, so the routed step launches without the restarted step's session.
- The relaunch passes the step's window, occupancy, and capacity admission as any start does.

The CLI resolves `--continue` from the selected attempt's supervised run `agent_id` and durable audit rows, and checks provider support, conversation presence, and the room account's history pool before staging. It passes the checked sequence, which rejects a moved attempt. Launcher preparation rechecks conversation presence and the room-default account's pool after the stop, so an account change in between cannot resume into a foreign pool. A resume failure after the stop ends the attempt `died`, which raises `died` at the step. The restart receipt shows the route only when the launch settled before it printed, and otherwise reports `awaiting launch`. A concurrent cancel can replace the ending and wins settlement.

The launcher keeps fresh identities and fresh companions, but compiles the leader's exec action as `Resume` when `LeaderRun.resume` is present. The shared fresh-cell compiler still installs the run id, workflow pin, reminder, managed account, and cleanup policy, and account resolution uses the room default. `agents restart` routes only current Step pins through this path and leaves team restarts alone. A former step's restart clears its pin and runs outside the workflow.

### Retiring a push-back before a stop

Before it stops an attempt, the host retires its pending push-back by message id, an unconfirmed `Sent` prompt included, through `Store::retire_message`. The queue writer cancels it with terminal history and audit evidence, and leaves other messages and delivered prompts alone. A producer that queues after the retirement re-reads the attempt's ownership and ending before delivery and retires its own obsolete push. So a continued restart's first clean turn is not gated by the abandoned attempt's push, while the current attempt's own unconsumed push still defers judgment.

## Inspection

Every view of a run is a projection of the record and the journal, and never transition authority. The [reference](../../reference/cli/workflow.md#inspect-runs) owns what each prints. What a change to a view must keep:

- `crates/rimz/src/harness/workflow/inspect.rs::hops` (the trail) and `crates/rimz/src/harness/workflow/inspect.rs::reached` (node entry counts) are shared by the CLI rail and the sidebar strip. Both take the record's held session: while a saved closing verdict waits on an ineligible leader, its evaluation projects no route, so the trail, history, and reached marks name neither its target nor a visit to it until the session closes. An evaluated check is marked reached even when its turn-end verdict was discarded.
- `inspect::attempt_superseded` folds a failed attempt into the failure route that follows it, so the trail prints one hop.
- `crates/rimz/src/cli/workflow/show/rail.rs::View::rail` paints the definition rows from the run's definition as it reads now, the journal, and the record. `crates/rimz/src/cli/workflow_view.rs::glyph_role` selects the headline and current-node glyph together.
- `--history` targets come from the run's definition, except where a turn-end verdict records its own target. When the definition cannot be read at a run's ending, `history::saved` still renders every journal row, without the route and verdict targets only the definition names, after one line saying the definition could not be read with the load error (left out when the record's reason already quotes it). Only a journal it cannot read leaves the header alone.
- The saved readings come from the latest evaluation since node entry and do not re-evaluate the check.
- `crates/rimz/src/harness/workflow/script.rs::condition_now` previews one condition check or conditional sleep for the `now` line: the first listed check on an agent step, or the current check on a live or parked run. It shares the engine evaluator, sleep expiry and the CI commit fence included, reads the forge cache once, and writes nothing: no child, no record write, no probe refresh. A sleep previewed from an agent step starts at the preview, so it never reads as slept. Preview freshness starts at step or parked-node entry, and held sessions keep their session-entry floor. JSON and history do not call it.
- `crates/rimz/src/cli/workflow.rs::RoomView::agent` reads the cached-alive roster `agents list` reads and projects the published pane frame over it, so a run's agent peers match the card's. Both views find an attempt's agent through one join, `crates/rimz/src/cli/workflow.rs::attempt_agent`, and use its card handle. Once a hook has bound the attempt's supervised run to a session, the agent is the live row of that session's launch (`address::launch_row`), so a `keep = true` agent whose detach cleared its pin keeps its card. Before that, it is the root pinned to the attempt's run and sequence as its step, the leader's recorded pet name preferred among layout cells. `RoomView::attempt_agents` applies the join to every attempt the journal ended with a supervised run and to the live one, giving `--history` the card handle of each agent still in the room (`crates/rimz/src/cli/workflow_view/history.rs::LiveAgents`) for attempt and signal rows; the fold key and the text `history::saved` writes at a run's ending keep the recorded handle. JSON and the loop views do no enrichment reads.
- `show`, `cancel`, `resume`, `extend`, and `restart` select runs through `crates/rimz/src/cli/workflow_view/resolve.rs::select`: caller-room matches win, otherwise several rooms require `--root`. Bare `show` resolves the calling agent and walks launched parents, with a cycle guard, to the first workflow pin (`crates/rimz/src/cli/workflow.rs::calling_run`). Run ids, the hidden `advance` positional included, resolve from any directory through `crates/rimz/src/store/workflow.rs::find_under`, which probes each recorded room's live and finished homes. It skips a room it cannot probe, unless that room holds a home of the id, and then returns the read error.
- `list` reads each run's journal for its rail. An unreadable journal leaves that row's reached history empty without failing the list.
- `crates/rimz/src/cli/workflow/show.rs::render` prints a live run whose definition admission refuses from its record alone, with the first error and the validate command, and `crates/rimz/src/cli/workflow/list.rs::render` names that refusal on the run's row ahead of a capacity hold. Neither prints other definition warnings.

`LiveAttempt` and `JournalEvent::Attempt` carry an optional `started_past_limit`: the hold text a `--skip-capacity` start skipped, without an age. `prepare` stores it on admission and `Transition::settle_step` copies it into the journal. `show` and history print it, and JSON exposes the field. No text goes to `workflow run` or `loop fire` output.

## Storage and isolation boundary

### The run home

Each room holds one home per run: `owned/workflows/<run-id>/` while the run is live, and `owned/finished-workflows/<run-id>/` once it has ended and owes neither its loop task, its starting agent nor the `runs` step that started it a report (`RunRecord::owes_report`). Every path in either directory comes from the disk path module (`crates/rimz/src/disk/paths.rs::WorkflowPaths`, `StatePaths::workflow`, `StatePaths::finished_workflow`):

```text
record.json        current run, version 19 (reads only 19)
journal.jsonl      append-only evidence: transitions, and a row per check evaluation, so a polling check grows it on every recheck
definition/        an inline run's generated definition: its text and assets (no deployed run has one)
history.txt        the history saved as the run leaves the live directory, written before the rename (a failed rename leaves it in the live home)
work/              RIMZ_WORKFLOW_DIR, writable by nodes
  context.json     replaced before every evaluation or attempt
  notes/           link to kept automation notes (runs without notes: scratch directory)
  signs/           live step guidance, created with the run
  <seq>.check.log  script stderr, or a test's combined output
  <seq>.step.log   command-step stdout and stderr
```

The record, an inline run's `definition/`, and the journal sit outside the node-writable `work/`. Notes are reached through the link in `work/`. Signs and step claims live under `work/`, and the engine reads only `signs/<step>.md`, never notes or claims.

Every evaluation and attempt takes a fresh `seq`, a re-run after a dead host included. The one exception is a message step's re-send after a dead host, which keeps its attempt's `seq` and message id so the queue takes the notice once. A node therefore keys a `notes/` write by the item it records: keying it by `seq` would not make a repeat a no-op.

Only the live directory answers "what is running". `crates/rimz/src/store/workflow.rs::live_runs` and every census, live listing, and protection read walk `owned/workflows/` alone; the FINISHED section of `rimz workflow list --all` (`crates/rimz/src/cli/workflow/list.rs::finished`) walks both directories of every recorded room for runs that ended, and a run found by id through `crates/rimz/src/store/workflow.rs::find` says which directory held it. Soft reset keeps run homes in both directories, and hard reset removes both. A home without `record.json` is a crash during creation (or while an inline run's definition is written), which every reader skips.

### Where a run reads its definition

`crates/rimz/src/harness/workflow/engine.rs::create` takes a `DefinitionSource`: a deployed directory, whose basename names the run, or an inline definition a loop task's fire builds, its text plus named asset files. An inline run's `workflow` is its task name.

- A deployed run keeps no copy. `create` records the directory it is handed, made absolute with its links and any `..` kept, as `RunRecord.spec_dir`, and an edit there reaches every live run of that workflow at its next engine read ([Engine reads](#engine-reads)). The run's name is the basename of the canonical directory. A pack workflow's path runs through the entry link, so a pack update reaches its live runs as an edit does, while the retired checkout lives until the next pack mutation collects it. A directory moved or deleted after the start, a source whose last pack was removed included, is a missing definition, and the run waits; an exclusion or a sibling pack's removal keeps the source's entry link, so the run goes on advancing. A symlink repointed after the start redirects the run. So an agent's start and a loop task's start hand `create` the claim's own path from `crates/rimz/src/harness/workflow/deployed.rs::Deployed::claim` (the home path or the pack's entry path), never the caller's spelling, and a link the caller made is never recorded.
- An inline run's text exists nowhere else, so `crates/rimz/src/store/workflow.rs::seal_definition` writes it and its assets into the run's own `definition/` before validation. Its record has no `spec_dir`.

Every reader takes the directory from `crates/rimz/src/store/workflow.rs::RunRecord::definition_dir`, which returns `spec_dir` or else the `definition/` of the run home the caller holds, live or finished. Prompts, scripts, argv paths, and `RIMZ_WORKFLOW_DEFINITION` all read it. Values the record decided at the start stay decided: an edit to the header's `deadline` or `scope` reaches later runs only. Agent profiles and machine configuration are read at each launch. `RunRecord.origin` records which source started the run (`deployed` or `inline`, and absent decodes as `deployed`).

### Notes folders

A run of checks alone allocates no notes folder when it is inline (a loop task's command run fires too often to keep a folder each time) or when no room exists, and its `work/notes/` is then a scratch directory. Every other run allocates `automation-notes/<yyyymmdd>/<HHMMSS>Z-workflow-<workflow>-<checkout>/` from the record's `created_at`, where `<checkout>` is the checkout directory's name, and stores its root-relative path in `RunRecord.notes`. Its v2 `run.json` names the workflow, run id, and scope checkout.

- Every step cell carries that path in `LaunchRecord.notes`, and launched descendants inherit it from their parent's row. Relaunches keep it and forks clear it.
- Claude's additional-directory grants include the run folder, the checkout's valid team run when present, and the home-wide note store once it exists, and reject paths outside the room's automation archive.
- Team reminders and memory listings keep resolving through the checkout's `.rimz/team` link.
- Agent-step briefs name `<step>-notes.md` unless the step sets `notes = false`.
- GC reads current workflow and agent ownership under the workspace lock and holds it through removal. Nonterminal workflows and live agent rows protect their folders, and an unreadable rollup protects all automation notes.

### Finishing a run

After CLI `advance`, `cancel`, or `restart` returns from the engine, `crates/rimz/src/cli/workflow.rs::finish_run` finishes a run that is terminal and owes no report:

1. It renders the run's history (`crates/rimz/src/cli/workflow_view/history.rs::saved`: the `show --history` renderer with raw prose, no colour, and log paths pointed at the finished home). A definition it cannot read then, as for a deployed run canceled without its directory, costs the history its route targets only ([Inspection](#inspection)).
2. It copies that history to the run's notes folder as `history.txt` when the folder exists.
3. It calls `crates/rimz/src/store/workflow.rs::finish` under the workspace lock, which rereads the record, writes `history.txt` into the home, renames the home into the finished directory without replacing anything there, and syncs both parent directories.

`finish` returns `Finished::Moved`, `Already` (a retry after a crash completes the barriers), `Gone`, `Kept` (unfinished, owing a report, or a newer RimZ's), or `Collision` (a home of that id is already finished, and the live one stays). A failed notes copy, or a move that fails before the rename, warns and leaves the home live, and the next advance, cancel, or restart retries it. A failure after the rename (a directory sync) leaves the home finished, and the CLI says the move completed but its barrier failed. Terminal history names the absolute local end in its headline and uses date-qualified local clocks (`YYYY-MM-DD HH:MM`) on every row, so the bytes are reproducible.

Paths recorded inside the home keep naming the live home, and the store's reads map the logs among them onto the home they read from, through `WorkflowPaths::relocate`. `crates/rimz/src/store/workflow.rs::journal` points each entry's logs at it (`crates/rimz/src/store/workflow.rs::JournalEntry::relocate`: an attempt's `log`, and the `log` and `detail.log` of an evaluation's object or a route's input), and `load` and `classify` do the same for the record's `input` and `prev_attempt.log`. So every reader, `workflow show` and the loop views included, names logs that exist, and on a live home the mapping changes nothing. `history::saved` renders before the move, so it maps the live journal onto the finished home itself. Nothing writes a mapped path back: `prev_attempt.claim_path` still names the live home, and a `context.json` a live handler read keeps the live paths it was given.

`workflow show <id>` of a finished run prints the header facts (outcome, reason, end time), the work directory in the finished home, and then the saved `history.txt`, and never reads the definition. `--json` carries the record, the journal, and the saved history. The record is the decoded one when `classify` finds the run `Current`, so its logs name the finished home, and the bytes as written for any other class.

GC removes a finished home once its `record.json`, or the home itself when it has no record, is older than the seven-day owned grace, whatever its version and without decoding it. The move keeps the record's mtime, so it does not restart that clock. When the run names a notes archive that lacks `history.txt`, GC copies the home's saved history there first, and a failed copy keeps the home. In the live directory GC removes only a recordless home past the same grace. A dry run predicts removal without writing a copy.

### The record and its version

The record (`crates/rimz/src/store/workflow.rs::RunRecord`) stores:

- the scope, project root, the deployed definition directory (`spec_dir`, present exactly when `origin` is `deployed`), and prompt
- the current node and its entry time, the state, and the routed input and key
- the open wait and its recheck time, per-step visit counts since the last reset with each step's split by item key (`key_visits`, charged beside the total by `Transition::charge_visit` and refunded by `Transition::withdraw_attempt`), the visit grant, and the deadline state
- the sequence allocator, the live attempt, an optional held session, the previous step attempt, and the terminal time

Optional fields carry:

- `origin`, the source, and `restart`, a staged restart
- `abandoned`, the runs of judges whose evaluation ended while their agent ran, owed a stop ([Judges](#judges))
- `once`, true for a try (`rimz workflow run --once`), omitted when false and never changed after the start
- two start arguments: `give_up`, the run's give-up instant for an agent step that sets no `admission.give-up` (`rimz workflow run --give-up`, and no loop fire sets one), and `capacity`, `skip` for a `--skip-capacity` run and omitted for the default `wait`
- the loop facts: `task` (the starting loop task), `manual` (true only for a run `rimz loop fire` created, omitted when false), `loop_report` (the one report mark, owed to the task, to an agent starter, or to the `runs` step that started the run), and `fire`, what the fire tells its briefs, frozen at creation: `manual` with the task's name for `rimz loop fire`, or `scheduled` with the task's name and the brief's trigger clause (none for a watch trigger)
- `outcome`, the typed run outcome, written with the terminal state ([Loop tasks](#loop-tasks))
- `starter`, the agent (by launch id) or the run and node that started it, omitted for a start by hand and for a task's run, and `depth`, `1` when omitted; `harness::workflow::starting` resolves both before `create`, and `Starting::admit` refuses a depth past `[agents] max-workflow-depth` before a worktree, a home, or a record exists ([starters and depth](../../reference/cli/workflow.md#starters-and-depth))

Each step's visits serialize as one integer under the step's name. Keys keep integer versus string identity. Unknown `Wait.hold` strings load as absent, and the shared [hold-kind contract](./loops.md#holds-and-their-kinds) covers workflow and condition waits alike.

Runs use version 19 (`store::workflow::VERSION`), the record's own shape version. It rises when a binary on either side of a change would misread the other's record. A change to what a definition means raises nothing, since every run reads its definition as it is at each engine read. `crates/rimz/src/store/workflow.rs::load` reads the version before it decodes the record and refuses any other version with one message naming the run and the version this RimZ reads. A change to what a stored record means raises `VERSION`, so a record an older RimZ wrote refuses rather than being misread. Raising it strands every unfinished run on disk, which the [workflow pass](#the-workflow-pass) ends.
`crates/rimz/src/store/workflow.rs::classify` reads a home's record as data, `RunClass`:

| Class | Meaning |
| --- | --- |
| `Current` | A run this RimZ runs. |
| `Ended` | A record whose `RunHeader` (the few fields read without decoding the body) says it ended, but whose body this RimZ does not decode. |
| `Stranded` | A run this RimZ cannot continue, with its `StrandedCause`: an older, missing, or invalid version, or a version-19 body that does not decode. |
| `Newer` | A newer RimZ's run. Its record is read no further than its version. |
| `NotObject` | A record that holds no JSON object. |
| `Recordless` | No record. |

Listings, the throttle's workflow census (`crates/rimz/src/harness/throttle.rs::SystemHost::workflows`), and `crates/rimz/src/store/workflow.rs::unfinished_under` take only current, unfinished runs and skip every other class without a warning. An I/O error reading a record still fails a strict read and the census. A command that names a newer RimZ's run by id refuses and changes nothing. A run this RimZ cannot continue is different: `workflow cancel` and `advance` on it end it as failed the way the workflow pass does (`crates/rimz/src/cli/workflow/reconcile.rs::end_named`), and `resume`, `extend`, and `restart` refuse. `workflow show` of the latter prints its header facts and that reason.

Engine callbacks that reload a run after off-lock work (`engine::reload`) treat a run that has left the live directory as ended. A late launch stops the agent it started, and a late message step retires its notice unless the moved journal shows it delivered. Neither appends to the moved journal nor recreates the live home.

### The journal and publication

A journal entry carries its time, node, optional attempt sequence, and one event (`crates/rimz/src/store/workflow.rs::JournalEvent`): hand-placed start, evaluation, attempt, visit reset, restart, turn-end decision, closed wait, explicit route, park, terminal, cancel, resume, extend, signal wake, or forwarded signal. A try's terminal entry carries `tried`, the target of the route it did not take.

- An evaluation keeps the routed object and its `origin` (`check` or `engine`, defaulting to `check` when absent). An invalid answer also keeps a stderr tail.
- Script and judge evaluations keep the digests published with the attempt's child, whichever process ends the attempt. An evaluation whose child never spawned has none.
- Attempt rows and `PreviousAttempt` carry the outcome that routed the run into the step in `routed_by`, beside the key, defaulting to null for older entries. A live attempt has no `routed_by`.
- Visit resets name the step and the answered outcome. Inspection takes reset boundaries from these events, never by re-evaluating a rule.

`engine::persist` is the publication path for every transition. Under the workspace lock it appends each journal entry and data-syncs the journal, then replaces the record, then writes accounting. The first replacement that makes the record terminal carries its typed outcome, and a live record carries none. A crash between the append and the replacement can leave a duplicate entry with the same sequence, which is harmless because history never authorizes a transition. A crash after the replacement can miss an assist. Creation syncs every new directory up to the room root. Readers skip torn or unknown journal rows.

## The advance loop

`engine::advance` is one loop of three phases, and each phase boundary is a lock boundary:

1. Prepare, locked (`crates/rimz/src/harness/workflow/engine.rs::prepare`). Load the record and decide: return, hold or restore a rest-checked session, stage an ending, route a due bound, reclaim a dead host's hosted attempt, count an early advance against a wait, merge a step admission wait, ask for capacity admission, or publish a new attempt with its `seq` and `context.json`.
2. Run, off-lock: evaluate the check, run a command step, ask the capacity door, launch an agent step (stopping a takeover's occupants once the launch is prepared), enqueue a saved push-back, or stop an ending agent step.
3. Settle, locked: reload the record, confirm the attempt's `seq` is still live, route, and persist.

A stale settlement writes nothing. One advance keeps going until the run waits, parks, ends, meets a live host, or would evaluate the same check twice with no step attempt or newer eligible rest between: that last guard is the hot-loop guard. A held verdict resolves before a wait can stop the advance. Repeated `visits` failures also yield rather than spin without launching. Each guard leaves the run active, and the next wake or tick drives it on.

### Engine reads

Each pass of `advance`'s loop reads the definition again and admits it before it prepares or settles anything. `Reader::read` in `crates/rimz/src/harness/workflow/engine.rs` loads `RunRecord::definition_dir` through `spec::load` and runs the same admission `create` runs (`crates/rimz/src/harness/workflow/validate.rs::admit_parsed`). The agent definitions that admission validates against load once per `advance`, and the hot-loop guard and the capacity admission survive each read. A hosted check's or command step's result is settled against a read taken after the child exits, falling back to the read from before the child when that one fails.

- A deployed definition that does not read, does not parse, or that admission refuses fails the `advance` without a write. The run waits, the scheduler tick retries it, and the first advance after the file is fixed proceeds.
- An inline definition that parses but is refused waits the same way. One that no longer parses ends the run failed with `this RimZ cannot read the definition generated at the fire: <error>`, since no later read can parse it either.
- `restart`, `resume`, and `extend` admit the definition through the same `Reader`, on the record they loaded under the lock and before they build their transition, so a refusal returns the definition's error with nothing interrupted, stopped, killed, or written. Unlike an advance, they also refuse an inline definition that no longer parses and leave it for the next advance to end.
- The signal helper admits each run the signal reaches, with agent definitions it loads once per wake, before it clears backoff, journals, forwards, or spawns. A refused run warns and is skipped.

In `prepare`, on the record read under the lock, and when a hosted check or command step settles, `crates/rimz/src/harness/workflow/engine/fit.rs::misfit` asks whether the run still fits the definition. A hosted child's once-a-second timeout read ([Hosted children](#hosted-children)) runs no fit check. The run fits when:

- its node is in the definition
- each live or held attempt sits at a node of the class it was published as: an agent attempt at an agent step with the same `lifecycle`, a message attempt at a message step, a command attempt at a command step, a judge at a judge, and any other check attempt at a check that is not a judge
- a held session, or a session with a push-back pending, sits at a step with rest checks, and a held session's judging check is still in its step's `rest-check` list
- a staged `asked` ending sits at a step that routes `asked`, and a held session's saved verdict that waits in a `for` hold names a case its condition still has (no other saved verdict is checked against the cases)

A run that no longer fits ends `failed` through `fit::close`, the operation cancel uses. It journals an engine `fault` evaluation naming the node and the reason, stops a live or held session as cancel does, and ends the run through no route, with the outcome reason `<node>: the workflow definition changed under the run: <what no longer fits>`.

### Hosted children

Test, script, and judge checks and command steps run inside the short-lived `advance` process, never as supervised runs.

1. Preparation publishes the live attempt (kind, sequence, node, start time, host PID and start token) and its context under the workspace lock, so a stale host cannot overwrite a newer attempt's context.
2. `crates/rimz/src/harness/workflow/script.rs::with_child` takes the lock again, re-reads the bounds, spawns the child, and publishes the child's group-leader PID, start token, and file digests in one write, before it releases the lock and waits for output. Spawn and publication thus share the lock with cancel and bounds. This write goes straight to the record (`workflow::write`), outside `persist`: it is no transition, so it journals nothing and writes no accounting.
3. While it waits, the host re-reads the record without the lock, at the bounded subprocess runner's interrupt cadence, and interrupts its child when the attempt is no longer live or a bound is due. The node's `timeout` comes from the definition as it is now (`script::Live`), read again at most once a second and before a passed timeout kills the child. A failed read falls back to the definition the attempt started under. The poll that kills the child records why (`script::Interrupt`: the deadline, the timeout, or the attempt ending), and settlement classifies the result by that cause.

Another advance leaves a live host alone while no bound is due. A check host that is dead, or whose PID was reused, turns its attempt into an engine-origin `canceled` evaluation, which re-runs free. A command step's dead host instead journals a `died` attempt with the reason `lost command-step host <pid>` and raises `died`, so the command's side effect is never repeated and no further visit is counted.

Any advance or cancel kills the recorded group under the lock before it withdraws the attempt, including when the host is stalled or dead (`Transition::kill_child`). It signals only when the leader's start token still matches, or, once the leader's token is unreadable, when a live member of that group carries this run's `RIMZ_WORKFLOW_RUN` marker. Membership is checked again after reading the marker and liveness. A missing or changed token alone never authorizes a signal. A hosted leader stays unreaped until its pipes close, so the published PID and token stay a valid kill handle. A surviving host reaps its child when it resumes, and its stale sequence cannot settle again.

A hosted child runs in the scope checkout with null stdin, the room workspace pin, `RIMZ_WORKFLOW_RUN`, `RIMZ_WORKFLOW_DIR`, and `RIMZ_WORKFLOW_DEFINITION`. Its `context.json` holds `run`, `prompt`, `node`, `seq`, `key`, `input`, `step_visits`, `key_visits`, `signs_path`, and `prev_attempt`. A session with a `rest-check` also publishes `live_attempt` ([Rest checks](#rest-checks-judging-an-open-session)).

A step's context adds `visit` (its visit count at admission, this launch or push-back included) and `visits` (its count now, since the last reset). The live attempt stamps `visit` at launch and at each admitted push-back. The context, `live_attempt.visit`, `prev_attempt.visit`, and the attempt journal row keep that latest number through resets alone, and an attempt without a stamp uses the current count. `prev_attempt` exposes the step, attempt outcome, `routed_by`, sequence, visit, key, start and end, claim path, handle, supervised run id, HEAD pair, stopped and surviving children, ask or limit evidence, cost, exit `code`, and `log`, with null for whatever was not observed. Signs, the run prompt, and the fire's evidence compose only agent prompts and message-step messages; a command reads the evidence as `RIMZ_LOOP_TRIGGER`.

### Command-step settlement

`crates/rimz/src/harness/workflow/state.rs::Transition::finish_script` records the end of every non-agent attempt, whichever process ends it: the hosting advance, a bound, a cancel, or a dead-host reclaim. The node table decides the journal shape: a step records `Attempt` and `prev_attempt`, and a check keeps the engine `Evaluation`. Agent and command steps share the attempt journal and the routing tail.

| Command ends | Outcome | Route |
| --- | --- | --- |
| Exit 0 | `ended` | `then`, with the original input |
| Nonzero exit, external signal, spawn failure, log-open failure | `died` | raises `died` |
| Step timeout | `timeout` | raises `timeout` |
| Run deadline | `canceled`, `EndCause::Deadline` | raises `deadline` (`Transition::deadline_failure`) |
| Cancel | `canceled` | terminal |
| Interrupted, but its bound was extended before settlement | `canceled` | raises `died` (`Definition::ending_cause`) |

Only an interrupted child is classified by the bounds at settlement. A child that exited on its own keeps its exit outcome, and the next prepare applies any run deadline still due. Another advance meets the same rule: `Transition::bound` leaves a step whose published child has exited to its live host, and that advance stops.

The hosting process samples HEAD after reaping, off-lock, and other settlers leave `head_end` null. `code` is null for a signal or an unobserved exit, and `log` is set only when the step log exists. Agent-only evidence is null, while `claim_path` still names the sequence's claim file. Command steps bypass `AttemptHost`, prompt composition, provider windows, and occupancy completely. Run steps pass argv to the hosted runner, and shell steps use `sh -c`. Both tee stdout and stderr to `crates/rimz/src/disk/paths.rs::WorkflowPaths::step_log`, `work/<seq>.step.log`.

### Message steps

A message step bypasses windows, occupancy, capacity, and the throttle, and never reaches `script::execute_step`.

1. `prepare` composes the text before it publishes the `LiveAttempt`, so the digests are recorded, mints `LiveAttempt.message`, and returns `Decision::Message`. A compose error takes the `died` path without a host call.
2. `engine::message_step` calls `AttemptHost::message` off-lock. The production host first looks the message id up in the live queue and history, and a hit returns the handle it was queued for. Otherwise it resolves the address from the scope's directory channel among living addressable agents, refuses a pinned `session` the address no longer names, queues `HarnessNotice::Wait` automated at gate `Done` with `queue_message_once` under that id, and attempts delivery at once.
3. It holds at `RIMZ_TEST_WORKFLOW_RESULT_GATE` (a test hook), relocks, and settles through `Transition::finish_message`: `ended` with the target handle follows `then`, and `died` with the reason raises `undelivered`.

When the attempt is no longer current at the relock, the advance writes nothing and retires its queued message, unless the journal already holds an `ended` attempt for that sequence. A dead host's attempt with a `message` id is re-sent under the same id by the next advance's `prepare`, not canceled, and the queue keeps one copy. The attempt journal row carries the id. The visit-cap check still precedes visit and sequence allocation. Nothing paces a message-step cycle, so a fast one can spend its cap within one advance: route it through a waiting check.

### Runs steps

A runs step starts other deployed workflows, each in its own worktree, and joins them (`crates/rimz/src/harness/workflow/engine/runs.rs`). It bypasses windows, occupancy, capacity, and the throttle: each started run's own steps take those.

1. `prepare` publishes a `LiveAttempt` of kind `workflow` whose `runs` (`StartedRuns`) reserve one run id per entry and the commit the run's checkout is at (`runs::reserve`), counts the visit, and returns `Decision::Runs`. The ids are reserved before anything is created, so a later advance that takes the start over cannot start an entry twice.
2. `runs::start` runs off-lock and loads the machine config once: an unreadable core config fails the attempt `died` with the `require_readable_core` message before anything is created, and the depth cap and the worktree settings read that same config. It pairs each reserved run with a live start entry by stored `workflow`, in order (`runs::pair`); a list that no longer pairs fails `died` naming the reserved workflows. A reserved run with `ended` set, or whose home exists, is never started again. `refuse` then checks a base commit, the depth through `Starting::admit`, and each pending entry naming a home or pack workflow (`crates/rimz/src/harness/workflow/deployed.rs::Deployed::find`, the lookup `workflow run` applies to a name), admitted, and not `scope = "project"`; only a first start, which no reserved run exists for yet, also requires a clean checkout (`git status --porcelain=v1 --untracked-files=all`), since a takeover finishes cutting runs from the commit its first start took. Then each pending entry gets a worktree from `worktree::create` with `WorktreeBase::Explicit(base)` and a run from `engine::create` under its reserved id, started by this run at this node (`RunStarter::Run`), one level deeper, owing a report (`StartArgs::join`). The first publish of a first start records the step assist. Before each worktree is created and again before each publish, `Attempt::recheck` rereads the definition and reloads the record under the lock: a run no longer current at the attempt stops the loop and is left to whoever settled it, a misfit is closed as at any read, and a passed bound settles as that bound. The worktree of a child not published after a failed recheck is removed and never announced; one that passes the second recheck is announced as `worktree.created` just before its run is published. A refusal or a creation error goes through the same recheck before it settles the attempt `died` with the reason, and runs already started keep running.
3. Each started run reports through `run_log/starter.rs::report`, which hands it to `runs::join_report` ([The report](#the-report)): it stores the ending on the reserving attempt's `StartedRun.ended` and spawns this run's advance. That write lives in the engine, not in `run_log`, because `run_log` sits in the sidebar import graph and must stay read-only.
4. Each advance's `runs::prepare` joins once every reserved run holds an ending, ahead of the bounds, so a join whose endings are all stored settles as the join even past the step timeout: `then` when each ended well (`state::ended_well`), else `died` with one reason naming each other run. The endings move to `RunRecord.joined` for the next node's `context.json` and brief. Until then the step waits as `runs`. When the attempt's host is dead and a reserved run was never created, the advance takes the host over and starts the rest.

A cancel, timeout, deadline, or `resume --at` ends the attempt and leaves the started runs running: they are runs of their own, with homes, records, and agents. A run started after the reserving attempt ended reports into nothing, since `join_report` writes only while the parent is active at that attempt. A crash between `worktree::create` and the child's publish leaves a worktree no run names. `engine::create` takes the lock itself to publish, so a cancel or a bound that lands between the second recheck and the publish still starts that one child; it then reports into nothing, as a run started after its attempt ended does. Started runs carry no `loop_fire`, so `loop stop` on the starting run's task does not reach them.

### Windows that stay open

Three races are accepted, each because closing it would open another.

- Only the hosting process writes the step log. When another advance enforces a bound on a stalled host, the next check can read the log before the host drains what the killed child wrote.
- A host killed after the spawn and before it publishes the child leaves `child` unset, so the dead-host reclaim kills nothing and the command can outlive its settled attempt. Checks share this window.
- A live host that stalls (SIGSTOP, a frozen cgroup) after its command's group leader exits holds the step. `Transition::bound` leaves it to that host, so no other advance enforces the step timeout or the run deadline, and a group member still running keeps running. The host's own bound kills the group once it resumes, and the dead-host reclaim kills it once the host dies. Waiting for the group to empty would close this window only by reopening another: `kill(-pgid, 0)` succeeds while the exited leader is still unreaped, so another advance would again overwrite the exit the host is about to record.

### Check answers

A script check saves its complete stderr to `work/<seq>.check.log`, creating the file even for an empty stream. Stdout is the answer, and an invalid evaluation keeps the journal's 4096-byte stderr tail.

A script or a judge answers exactly one JSON object: a required string `outcome`, an optional string `reason`, an RFC 3339 `recheck_at`, and a string or integer `key`. Other fields stay in the object as payload. The full normalized object (`crates/rimz/src/harness/workflow/state.rs::normalize_object`) becomes the next node's input, and a missing key inherits the run's current key. A sleep journals its own answers but forwards its incoming input and key on `then`, so the next step's `routed_by` still names the incoming outcome. A sleep's `deadline` failure supplies the failure input instead, keeping the inherited key.

For a script, a nonzero exit, a signal, a process that could not spawn, and a `run` file that no longer resolves raise `died`. Stdout that is not one object, a wrong field type, an unrouted or reserved outcome, and a bad `recheck_at` raise `invalid`. A failure once `.check.log` exists carries `log` in `detail`, plus `code` once the process exited. A test or script whose process could not spawn carries `log` alone. A refused answer settles through `Transition::finish_check`, which takes the same detail. A judge's failures settle differently ([Judges](#judges)).

`recheck_at` only delays a `@wait`. `Transition::bounded_recheck` clamps it to the earliest of the check's timeout, a held session's timeout, and the run's unfired deadline, so the tick cannot sleep past any of them.

The push-back brief looks backward for the `TurnEnd {pushed}` matching `attempt.push.message`, then for that check's preceding evaluation. It names that check and, for a test or script, measures its `.check.log` with `FileSummary`. A judge's push-back has no output line: its answer is the whole account. The reason comes before the output path, and an empty or missing file says `no output`. No file content is copied. Without a matching row it falls back to the step's first check and leaves out the output line.

### Test checks and command steps

A test check and a command step run through the loop's `crates/rimz/src/harness/check_run.rs::run_command` in its hosted mode, with no SIGINT listener. A script check does not: `script::evaluate` spawns it directly, with stderr to `.check.log` and stdout read as the answer.

- A `shell` test or step, a loop fire's `command` check included, goes through the `rimz loop check-exec --command` trampoline, and a `run` test or step through `rimz loop check-exec -- <argv>`, which execs the argv without a shell.
- The host first resolves `sh` or the `run` program on the child's `PATH`, so a missing program stays a spawn error rather than the trampoline's exit.
- The trampoline calls `setsid`, so the command leads a new session and process group with no controlling terminal, and a command that opens `/dev/tty` cannot stop on the caller's job control.
- Before polling, the spawn callback publishes the leader under the workspace lock, and a callback error kills the group and returns the error. A child killed before its trampoline reaches `setsid` leads no group yet, so `Transition::kill_child` falls back to the leader's PID when `killpg` finds none.
- The interrupt predicate runs every 20 ms, kills the group at once, and bounds pipe draining to 200 ms. A test's output is teed to `work/<seq>.check.log`, and a command step's to `work/<seq>.step.log`.

A test's answer comes from the exit: `pass` for exit 0, otherwise `fail` with a reason (the [reference](../../reference/cli/workflow.md#json-shapes) owns the shape). A `run` test publishes the digests of its slash-word files, and a `shell` test none. No tail is journaled. A spawn or log-open failure raises `died`, and so does a command a signal killed (`crates/rimz/src/harness/workflow/script.rs::evaluate_test`), with reason `killed by signal` and a `detail` holding the `log` and no `code`. The workflow's own timeout and deadline interrupt through the same predicate and raise `timeout` and `deadline`, never `fail`. No loop default timeout applies.

### Conditions and holds

A condition runs the same prepare, evaluate, and settle protocol with kind `script` and no hosted child (`crates/rimz/src/harness/workflow/script/condition.rs::evaluate`). Each case's truth is the shared three-valued walk `crates/rimz/src/harness/when.rs::WhenExpr::truth` over the readings: the first true case wins, an unknown case before it answers `unknown`, and all false selects `else`. The answer carries the winning case index (omitted for `else` and `unknown`), the inherited key, the readings after the forge floor and the commit fence, and, for `unknown` only, a reason naming each unread key. Its origin is `check` and its digests are empty.

#### The forge floor

Every forge reading is cleared unless the latest probe succeeded at or after the forge floor. A held evaluation's floor is `held.entered_at`, the session's step entry, and every other evaluation's is `record.entered_at`, the check's entry. A held floor's unread reason names the session's start rather than the check's entry. The check timeout still uses `record.entered_at`, the hold time while held. Stage and projected usage-window readings have no floor. A closed room leaves the last facts that pass the floor in force.

The floor has two latency costs: a check can wait about five minutes at idle probe cadence, and consecutive forge checks each wait for a new probe.

#### The CI commit fence

After the floor passes, bare `ci` on an open PR link with a nonempty `open.head` is fenced by commit, in two rules:

1. A readable local HEAD that differs from the PR head makes `ci` unread only when `git merge-base --is-ancestor <local> <pr_head>` exits exactly 1.
2. Otherwise, a known, nonempty `ci_sha` that differs from the PR head makes it unread.

Rule 1 wins when both apply, and the floor's reason precedes both. The [reference](../../reference/cli/workflow.md#json-shapes) owns the two reason forms. An unreadable local HEAD skips rule 1, not rule 2. An ancestry exit 0, 128 (an unfetched PR head included), any other exit, a timeout, or a spawn error also leaves rule 2 to decide. An unknown `ci_sha` skips only rule 2. Without a PR head (legacy empty heads, merged or closed links, Gitea, a GitHub facts miss) the floor alone applies. Branch selectors, `pr`, `pr.queue`, `pr.mergeable`, `pr.review`, and PR-less `branch_ci` have no commit fence and stay subject to propagation lag. Loop conditions share the floor and the fence through `crates/rimz/src/harness/when.rs::forge_unread` and `crates/rimz/src/harness/when.rs::CiSource::ci_fence` ([loops](./loops.md#condition-planning)).

The evaluator reads the cache once, then reads fresh local HEAD in the scope path. Both git reads live in `CiSource::ci_fence`, bounded at two seconds through `crates/rimz/src/proc/mod.rs::run_bounded_git_output`. They only read, and the sidebar graph reaches them through loop conditions. Git runs only for a case naming bare `ci` after the floor passes and an open PR head is known, and ancestry runs only when local HEAD differs. Settlement under the workspace lock does not re-read either input: the answer describes the cached PR head at `checked_at`, and a later commit or push is a new fact for the next evaluation.

No probe cadence or signal accompanies the fence. A local HEAD change nudges one probe, and if it beats the forge, the next probe uses the repository TTL (20 seconds hot, 300 seconds idle). CI signals fire only on verdict changes, so `failed` at an old commit becoming `failed` at the new one is sampled by the minute workflow tick, not a CI signal. The wait therefore adds up to a tick after the cache catches up.

#### Holds

Settlement under the lock decides holds, never the off-lock evaluator. A winning case with `for` merges a wait keyed by `(outcome, case)`, with outcome `hold`, the case's outcome as detail, and the case index in `Wait.case`. A hold keeps the run's routed input and key. Once `now - first >= for`, the case routes its own outcome.

An `unknown` answer routed to `@wait` keeps the hold open with its `first` when the held case reads true or unknown on that answer's readings (`Transition::hold_survives`), so a later true answer of the same case continues it. While it survives, `wait.unknown` keeps the unknown answer's reason, and every reader describes the node as unknown, not holding: `workflow show` prints `unknown: <keys>` without the hold's forecast, the rail marks cases from that answer's readings, and the sidebar strip reads `unknown`. The held case's next true answer clears it. Any other answer closes the hold: the held case reading false, another case answering, or a route that leaves the node. A later true case then starts it again.

Ordinary check evaluations recompute the bounded `first + for` recheck, so a wake keeps `first`. The tick re-samples an ordinary hold before its recheck time, so a transition that falls entirely between two samples goes unseen. A held rest-check wait also rechecks on subscribed signals and the minute tick while its leader rests on the judged turn.

### Sleeps

A sleep evaluates in-process with no child, digest, or stderr. Its `sleep` duration runs from `record.entered_at`, so entry through a route or a parked resume restarts it. Expiry is checked first and answers `{outcome: then, reason: "slept <sleep>"}`. Before expiry, a plain sleep answers `{outcome: sleeping, recheck_at: <entered_at + sleep>}`. That stored recheck is clamped to an earlier deadline, and the tick does not spawn before it, so a plain sleep journals just its entry and expiry evaluations.

With `when`, `Check::condition_cases` compiles one derived case into the node with outcome `then` and the optional `for`. The condition evaluator then supplies freshness, unread handling, subscriptions, and hold settlement, with fallback `sleeping`. `sleeping` and `unknown` store no recheck, so the minute tick keeps sampling and matching signals wake it earlier. Expiry may be about a minute late, and it wins over an unfinished hold. `for` refuses when invalid, zero, without `when`, or at least `sleep`.

Sleeps have real `sleeping → @wait` and, with `when`, `unknown → @wait` edges, so they bound check-only cycles without a timeout. Definition views hide those default edges and the derived case, and print `sleep`, `when`, `for`, and `then` instead.

### Waits

A wait keeps its node and the node's original timeout clock. Repeated identical waits merge in the record (`first`, `last`, `count`, `recheck_at`), and leaving the wait appends one summary to the journal. A check wait's span runs from the answer that opened it to the newest answer merged into it. An advance that arrives before an ordinary wait's `recheck_at` increments its count and moves `last` without evaluating. A hold is re-sampled instead.

### Bounds

Every advance checks bounds before routing. An agent observation first updates its live ask hold. Each bound raises a failure at the node with the failure input, which inherits the current key:

| Bound | Fires when | Detail and reason | Cause |
| --- | --- | --- | --- |
| Deadline | Once, when the unfired deadline passes. It stops gating until `extend --deadline` re-arms it. | `{deadline: T}`, `the run's deadline <T> passed` | `deadline`, at the node the run is at |
| Check timeout | The check's `timeout` after `entered_at`, spanning every evaluation and every other wait at the node. A judge's runs from its attempt's start instead and is never due before its launch, but no hold pauses it. | `{}`, `<C> ran past its timeout <S>`; a judge stopped when it passes adds what it was stopped on ([Judges](#rest-holds-and-endings)) | `timeout` |
| Step timeout | An agent step's `timeout` after the attempt started, less closed hold time (`asked_ms`). Never due while a hold is open (`held_since`) (`crates/rimz/src/harness/workflow/inspect.rs::timeout_at`). | The attempt's evidence, `the step's timeout passed` | Ends the attempt as `timeout`, which raises `timeout` |
| Session timeout | The same clock on a session held at its rest check, read from `held.attempt` (`crates/rimz/src/harness/workflow/inspect.rs::session_timeout_at`). Held preflight (`crates/rimz/src/harness/workflow/engine/rest.rs::prepare`) checks it after the deadline and before the check's own timeout, so the session's timeout wins when both are due. | The attempt's evidence, `the step's timeout passed` | Abandons the check and ends the session as `timeout`, which raises `timeout` at the step |
| Visits | Before launch, the step's visits since the last reset reach `max-visits` plus the step's grant. Nothing launches, and no visit or sequence is spent. | `{visits: N, max: M}`, `<S> has used N of M visits`, with the grant included in `max` | `visits`, at the step |

`rimz workflow restart` starts a step's clock again from the relaunch.

## Agent attempts

Agent steps launch through `launcher::prepare_layout` and `launcher::execute`, with room Birth and a detached leader run (`turn` for turn steps, `session` for sessions). They open an unfocused new tab titled `run <leader kind>`, never as the advancing agent's children.

- The leader keeps supervised preflight and defaults an unset permission mode to Auto. Companions use layout preflight with loop-equivalent trust verdicts and workflow-named refusals.
- Every agent cell carries `WorkflowAttemptRef {run, seq, role}` and pins `RIMZ_WORKFLOW_RUN`, `RIMZ_WORKFLOW_DIR`, and `RIMZ_WORKFLOW_DEFINITION` under every isolation level. Only the leader has a run. Command cells follow its durable record and close themselves after it settles.
- RimZ-launched children do not inherit the attempt identity, but do inherit the separate [automation stamp](../agents/model.md#the-rollup) naming the workflow and launching step.
- `LiveAttempt.lifecycle` is `turn` or `session`. It decodes a missing value as turn and omits turn on write.

The shared launch compiler (`crates/rimz/src/harness/launch_plan.rs::compile`) takes `RIMZ_WORKFLOW_DEFINITION` and the Environment `- workflow:` line from the pinned run's `crates/rimz/src/store/workflow.rs::RunRecord::definition_dir`, independently of `env_reminder`. A failed record read returns a launch warning and omits both. Non-workflow launches unset that variable alongside `RIMZ_WORKFLOW_RUN` and `RIMZ_WORKFLOW_DIR`.

### Admission

Every agent start, whatever its origin, passes admission in this order: bounds, the visit cap, each agent cell's provider window, the room's and each cell's account budget, checkout occupancy, the workflow and step limits, the machine FIFO when machine-gated, and both machine caps. Unless the step sets `admission.throttle = "off"`, the start also checks machine stagger, resources, and the capacity script. The caps and the workflow and step limits apply to an `off` step too.

One value decides whether a start waits for capacity: `prepare` reads `RunRecord.capacity` once and hands it to `prepare_capacity`, so the admission errors `prepare` keeps and the holds `prepare_capacity` waits on cannot disagree. A `skip` run launches past every capacity hold and capacity admission error, except a start too wide for a machine cap, and records each hold on the attempt. It still waits on the provider window, a budget, a takeover, and an occupied checkout, which `prepare` decides before it reaches capacity.

Each blocked window, budget, occupancy, or capacity check merges a wait that keeps the routed input and key and spends no visit or sequence:

| Blocker | Wait |
| --- | --- |
| The step's `window` is closed for any agent cell's provider. | `window`, with the blocking term and its reading in `Wait.detail` (`5h.left=12<42`). |
| The launcher's budget gate would refuse the launch: the room's budget, or the account budget of a cell's resolved login (`crates/rimz/src/harness/workflow/engine.rs::budget_refusal`, read for a judge too). The provider window gate needs a prepared launch, so the launch answers for it. | `budget`, with the gate's reason in `Wait.detail`. |
| An occupant is mid-turn. | `takeover` |
| An idle occupant, and the step's `admission.occupancy` is `wait` (the default). | `occupied`, with the occupants' handles joined by `, ` in `Wait.detail` (`@otter, @fox`), rewritten at each recheck. The views print `occupied by @otter, @fox`; the sidebar strip keeps the bare word. |
| The launch refused on reporting hooks or folder trust the user can fix in place (`NotLaunched::Trust`, [Launch retry](#launch-retry)). | `trust`, with each problem and its fix in `Wait.detail`. |
| The shared start throttle or a workflow or step limit holds the step. | `capacity`, with `Wait.kinds`, `Wait.detail`, an optional typed `Wait.hold`, and an optional `Wait.gives_up_at`. The kind survives retries and closing the wait into history. |
| Admission times out waiting for `loops/throttle.lock`, and no capacity wait is open. | `capacity`, held as `queue`, with `waiting for the start queue: <lock error>` in `Wait.detail`. |

#### Re-reading the bounds

Prompt composition and `git rev-parse HEAD` run under the lock after the first bound read, and they take time. So `prepare` takes a fresh clock after them and reads the bounds again, and a bound that came due meanwhile routes without publishing an attempt, spending no visit and no sequence. Only after that second read does an admitted launch count the visit, record HEAD, and publish its context and live attempt, stamped with the fresh clock. The bounds are re-read rather than the work moved off-lock, because the launch decision and its publication share the lock on purpose. The window reading is not re-read after prompt composition: it was sampled in the same pass, and provider windows move in minutes.

#### Occupancy and takeover

Who occupies a checkout follows the [shared occupancy read](./loops.md#takeover), `crates/rimz/src/harness/takeover.rs::read`, which leaves out the run's own agent starter while the run owes it the report (`RunRecord::owed_starter`, passed through `AttemptHost::occupancy` and `Eviction::beside`): that agent waits on the run, so counting it would hold both. `admission.occupancy` takes three values (`crates/rimz/src/harness/workflow/spec.rs::OccupancyPolicy`):

- `wait`, the default, waits for the occupants.
- `takeover` stops settled occupants and waits `takeover` while one is working.
- `share` skips the occupancy read and eviction entirely and never records a `takeover` or `occupied` wait. Window, capacity, and visit bounds still apply. A sharing step's agent still counts as an occupant for later steps that do not share.

At the project root, owned nested worktrees are excluded. A step with a `worktree` destination reads occupancy, and evicts, at that destination rather than at the run's scope (`crates/rimz/src/harness/workflow/engine.rs::prepare`), and a takeover there relaunches the destination's stopped team cohort fresh rather than resuming it. With `takeover`, capacity is decided before eviction, so a capacity-held step never stops an occupant. The capacity admission request runs off-lock in its own round trip, and each return re-reads bounds, visit caps, window, and occupancy.

Eviction is part of the launch (`StepLaunch.evict`). The host prepares the launch first, so a launch its preparation refuses (a saved-untrusted folder, untrusted hooks) stops no one. Then `crates/rimz/src/launcher/workflow_host.rs::Host::evict` reads the checkout again, even when the first read found it free, and finds it free only by a read taken after the last stop it issued:

1. A read that finds settled occupants stops them through `launcher::stop::stop_resolved`, and reads again. This repeats up to `EVICT_STOP_ROUNDS` (3) rounds, so an occupant that arrived while the launch was prepared is stopped too.
2. A read that finds any other occupancy (a working occupant), or occupants still there once the rounds are spent, returns that occupancy. The engine takes back the attempt and the visit it counted (`Transition::withdraw_attempt`) and waits `takeover` or `occupied` as the first read would, with no failure and no strike.
3. A free read executes the prepared launch.

`crates/rimz/src/launcher/stop.rs::stop_resolved` attempts the whole set, waits for the closed rows' processes, sends SIGTERM to eligible survivors, and waits again (`Stopper::judge`). An occupant counts as stopped only when a read after those waits finds no pane hosting it and its process gone. A failed close with no hosting pane counts as stopped when a fresh read finds the owner `Dead`, or `Unknown` with an end stamp or a failed unbound launch (`stop::unhosted_verdict`). [Stopping an agent tree](./fleet.md#stopping-an-agent-tree) owns the cascade. Any other failed stop fails the eviction, and the step launches nothing.

Nothing makes the occupancy read and the pane closes atomic, which is why the host reads again after every stop round.

#### Launch retry

A launch whose eviction failed, reported as `NotLaunched::Unstarted`, started nothing and is retried once per step entry (`crates/rimz/src/harness/workflow/engine/agent.rs::launch`). The engine withdraws the attempt and its visit, sets `RunRecord::launch_retry`, and waits `retry` with the reason in `Wait.detail`, which the journal keeps when the wait closes. The next advance (a tick or a hook wake) launches again with no `recheck_at` floor. A continued restart (`StepLaunch.resume`) is not retried.

A launch the launcher's own budget gate refused, reported as `NotLaunched::Budget`, started nothing either: the budget was spent between `prepare`'s read and the launch, or a gate the read leaves to the launch refused. The engine withdraws the attempt and its visit and waits `budget` as the first read would have, keeping a pending `RunRecord::restart` for the launch after the wait. It sets no `launch_retry`, so the wait repeats until a launch is admitted. The withdrawal closes no wait: `prepare` closed the earlier one at launch, so the new wait's `count` starts at 1. This wait, the `takeover` or `occupied` one after a failed eviction re-read, and the unstarted retry are taken only when no ending was staged on the attempt while it launched off the lock (a cancel, a restart, a misfit). With one staged, the refusal goes down the launch-failure path, which keeps the staged ending, and `finish` settles it.

A launch the host refused on reporting hooks or folder trust the user can clear in place, reported as `NotLaunched::Trust` (`crates/rimz/src/launcher/workflow_host.rs::Host::launch` maps a clearable `crates/rimz/src/launcher/trust.rs::TrustRefusal`), withdraws the attempt and its visit and waits `trust` with the fixes in `Wait.detail`, for a step and a judge alike. It sets no `launch_retry`, so each advance launches again, and only the run deadline ends the wait. The wait keeps its first stamp across rechecks: the step publish in `engine::prepare` skips `close_wait` while a `trust` wait is open, and `engine/agent.rs::launch` closes it once the launch has an outcome, so a later admitted launch journals one wait whose `count` is its rechecks. A definition edit that leaves the waiting node launching neither an agent nor a judge ends the wait in `engine::prepare` instead, and `workflow resume --at` closes it before it moves the run. `RunRecord::needs_you` covers it. With an ending staged, it takes the launch-failure path as the others do. `workflow run` reads the same verdicts before it creates a run (`crates/rimz/src/launcher/trust.rs::workflow_start_problems`), so this wait covers what changes after the start.

These become a counted `died` attempt, which takes the failure chain:

- a second unstarted failure
- any other host-reported failure: a refusal out of `launcher::prepare_layout`, or an error out of `launcher::execute`
- a launch-group resolution error
- a waiting start's admission error other than a queue-lock timeout

An agent group wider than a machine cap raises `capacity` before it counts a visit, and takes the chain the same way.

#### Capacity and the throttle

`advance` asks `throttle::try_admit` at most once per next-sequence and node pair and carries its answer into the next locked `prepare`. A route can change the node without allocating a sequence, so either part changing discards the answer and its turn. The kind and account grouping uses the same launch-cell resolution as the window. A held answer leaves no ticket and persists `step_wait("capacity", reason)`, and the minute tick retries it. [The start throttle](./loops.md#the-start-throttle) owns the queue, tickets, the capacity check, and the agent-row census.

A queue-lock timeout (`ThrottleError::Lock(LockErr::Timeout)`), at any of admission's four queue-lock scopes (two in `throttle::try_admit_in`, two in `throttle::check_groups`), is contention, not a fault. `prepare` keeps it out of the launch error, and `prepare_capacity` records it as a capacity wait held as `queue` for a waiting run, which gives up through the same chain as any other capacity hold. A `skip` run launches past it. When a capacity wait is already open, the timeout learned nothing, so the wait keeps its hold, kinds, reason, `first`, and give-up time, and a `workflow_limit` or `step_limit` waiter keeps its FIFO standing. Lock open and acquire faults, queue and ticket I/O, reading and ticket-write failures, and a vanished ticket stay counted `died` attempts.

`Wait.first` survives consecutive capacity holds, and any other wait outcome restarts it. The give-up time is the node's `admission.give-up` (a duration from `Wait.first`, or `forever`), else the run's `give_up` instant, else none, and `forever` on the node wins over the run's instant. A time already past at the first hold gives up on the same advance. At its give-up time, `crates/rimz/src/harness/workflow/engine.rs::capacity_failure` raises `capacity` with `detail: {waited_ms, first}` through the failure chain, and the run deadline stays an independent bound.

`prepare` also keeps `ThrottleError::Oversized`, a start wider than a machine cap, out of the launch error. `prepare_capacity` raises `capacity` from it through `capacity_failure` before it counts a visit, on every run that reaches admission (`waited_ms` 0 unless a capacity wait was open), with `never_fits: true` in its detail. `engine::failure` reads that member: a run such a route ends fails like any other failure, so a loop run that ends there reports `failed` and strikes, where an expired wait reports `throttle skipped`.

Admission carries its optional turn through `StepLaunch`, `LeaderRun`, and `prepare_run_layout` to `SupervisedRunRequest.throttle_turn`, which `open_fresh` reports. Any other ending drops the turn. Command steps, checks, and existing sessions never take a turn.

A `skip` run (`throttle::Run::skip`) passes every hold its start meets (the limits, the queue, the machine limits, and the capacity check), records each as text joined by `; `, and claims an `Admitted` ticket. The skipped pass shares the waiting pass's `launching` and `limits::evaluate` expression under the queue lock, before it writes the admitted ticket and before any launch stamp moves. A census or stamp read failure and a capacity check that errors or times out also become notes. The admitted `Turn` carries the skipped hold into `prepare`, and held, open, or errored answers carry none. No assist is written: the start is the user's action.

#### The limit pass

`crates/rimz/src/harness/throttle/limits.rs` owns the locked limit pass. The request carries the workflow name, step, asking run id, capacity arrival (`Wait.first`, or the current request instant), and the limits the run's definition sets at that read. Whether the start waits is not part of the request: `throttle::Run::skip` decides it in the pass.

Admission first refuses an automated group larger than a machine cap, naming the cap and its fix. A workflow or step limit counts the start as one whatever its width, so it refuses none. Otherwise, under `loops/throttle.lock`, the pass checks in order:

1. an in-flight turn of the same limited workflow
2. FIFO
3. the workflow count, then the step count
4. workflow stagger, then step stagger
5. the machine queue, when machine-gated
6. both machine caps
7. machine stagger and resource checks, when machine-gated

Every limit hold, the in-flight one included, is reported under its limit kind before any machine hold, so a waiter keeps its FIFO standing on a pass where the machine also holds it. Each machine cap requires the whole incoming agent group to fit, and other admitted-but-unlaunched starts reserve their agent counts under the same lock. No workflow or step limit bypasses a machine cap.

A same-workflow admitted turn keeps limited starts serialized until its launch report. `engine::prepare` publishes the attempt under the workspace lock before the launch reports, so from the report on the run record carries the count, and the next `sweep` drops the launched ticket's limit scope without reading any agent row. Between admission and publication the admitted ticket holds, and from publication the record counts, since a published attempt with no readable row or no turn closed since it started counts as working, so no instant leaves an admitted start counted by neither.

FIFO reads Active same-workflow capacity waiters ordered by `(Wait.first, run id)`: `workflow_limit` holds every step, and `step_limit` only its matching step. A free slot can stay idle until the earliest waiter's next minute tick. Hold text prefixes `workflow <name>: ` or `step <name>: ` to `N of M agents working (@a, @b, +K)`, `next start in <duration>`, `N earlier start(s) waiting`, or `N start(s) launching` while a same-workflow launch is not yet counted (named for the workflow limit when one is set, else the step). Machine cap reasons name multi-agent demand separately (`this start opens 2`). Workflow and step reasons name each counted run by its recorded leader handle (`AttemptRun::handle`), at most two and then `+K`, and an attempt without a recorded leader counts only in `+K`.

#### The run census

The limit census starts from run records: `crates/rimz/src/harness/throttle.rs::SystemHost::workflows` loads every run record of every known room and keeps the nonterminal ones. A run a configured count would include then asks `Host::waits_for_user`, which `SystemHost` answers through `crates/rimz/src/harness/workflow/observe.rs::session_waits_for_user` from the room's own store.

- Workflow scope counts one per run of that workflow name whose record holds an agent session (`RunRecord::session_attempt`) that does not wait for the user. A judge attempt does not count.
- Step scope also matches the session's own node (`LiveAttempt::node`), not `RunRecord::node`, which names the check while a session is held.
- The asking run's own record is skipped by run id, and the start it asks for adds one.
- Companions, RimZ-launched children and peers, and kept agents that left their run add nothing, since no record attempt names them.

A session waits for the user when the rest check's reading holds (`observe` with `Observation::Census`, and the shared `Observed::resting_turn` that `engine/rest.rs::eligible` also takes) with three differences, each stated in `session_rests`:

- An interrupted turn rests, since that session waits for the user; a failed turn, a provider limit, a budget park, and a blocking ask (the leader's or a launched descendant's) keep the slot.
- Every live companion the attempt pins must rest with no displayed error, no debt, and no blocking ask. A companion that never opened a turn rests.
- Pending ingress of any agent of the workflow run defers rest, read with no lock by `store::ingress::peek_pending`, and an ingress read that fails counts as pending.

The record must also allow rest (`resting_attempt`): a session lifecycle, no ending, no staged restart, no push-back prepared and not yet queued, and, for a session held at its rest check, no live check attempt, no saved verdict, and a wait a check answered (`Wait::answered_by_check`, against the engine's own wait outcomes in `Wait::ENGINE_OUTCOMES`). The leader must have a row, not be `Dead` by `store::runtime::agent_liveness`, and have closed a turn no earlier than `LiveAttempt::started_at`, since a resumed or restarted session carries its old closed turn on its row. A record that names no readable supervised run counts. The cheap row terms run before the budget projection, the ask reads, the debt reads, and the ingress read, since the census runs under the queue lock for every waiter.

Every read takes no lock, runs no migration, and writes nothing, so the census can read another room from any process: the cached snapshot, the run and message records, the task catalog, the open ask's transcript buckets, and the budget caches. The reading can be stale by the time a start launches. A session the user answers counts again only once its prompt hook reaches ingress, and a start admitted before then goes past the limit, which a limit accepts as it is not a hard ceiling.

An unreadable required room or stagger record holds a scheduled start as `store`, never as a short count or an absent stamp.

#### Start stamps

`Turn::report_launch` alone stamps `loops/start-stamps.json`, resolved by `crates/rimz/src/disk/paths.rs::loop_throttle_start_stamps`, under the same queue lock as the launched ticket. The record has `machine` (an optional launch timestamp in milliseconds) and `workflows` (workflow name to `last_launch_ms` and a `steps` map of step name to timestamp). Only machine-gated launches set `machine`, and limited launches set their workflow and step stamps. It uses the queue's rename-atomic cache writer without per-launch fsync, so a process crash keeps completed replacements, while power loss can lose the newest queue or stagger write. A missing file means no prior launch. When scheduled admission needs a configured stagger, a malformed or unreadable file holds as `store`. An unreported dropped turn stamps nothing. Skipped launches reserve in-flight scope and restart the stamps of every scope they are gated by without waiting, and `off` launches never restart machine stagger.

### Prompt composition

`crates/rimz/src/harness/workflow/engine/agent.rs::compose` joins four texts with blank lines at every launch: the prompt file, `signs/<step>.md` when present, the run prompt, and the fire's evidence (`RunFire::evidence`) when the run's fire carries one. A sign's canonical path must stay inside `work/`. The digests are keyed by the spec's prompt-file spelling, `signs/<step>.md`, `prompt`, and `evidence`, and absent optional text has no digest. A relaunch can find a dirty checkout left by a stopped attempt, and the step's prompt must handle it.

`crates/rimz/src/harness/workflow/brief.rs::step` composes a separate `### Workflow` launch-reminder body off-lock in `engine::agent::launch`, from the definition the launch read and the published, bumped run record.

- It reads the journal through the same `routing_hop` rule as `show`. A `Start` hop at the node says the user started the run there by hand, never that it is the run's first step, and names a given input as `input` in `context.json`.
- It words the run's origin from what the record froze at the fire (`RunRecord::fire`: the task's name, and a scheduled fire's trigger clause), never from the task catalog or `RunRecord.task`. Editing, renaming, or removing the task mid-run leaves the sentence as it was, old task name included.
- A resume counts as the sender after a park, even before its agent launches, and an `extend` does not replace that sender. Unreadable enrichment drops only the arrival sentence.
- The rail follows `Definition::walk`, groups returning steps beside their check, and draws arrows only for live non-deadline routes. Unreachable nodes stay out even after an explicit resume there. Visit bounds include each step's grant.
- The leader's check descriptions follow the step's `rest-check` order, then its `then` check, each once. Conditions keep their cases, scripts and shells show the command plus an optional summary and their execution and path rules, and judges show only a summary. Script and shell descriptions explicitly allow the agent to run the command itself.

`StepLaunch.workflow_note` travels through the leader's supervised request as `ReminderSection::Workflow`, never as a loop section. Companions get no section, and a step's subscriptions are its own `subscribe` wakes. `crates/rimz/src/harness/workflow/engine/agent.rs::step_launch` gives a judge the shorter `brief::judge` section. The notes point models at workflow views, not raw context files. Ordinary agent resume refuses current workflow pins, and rebirth skips them. A workflow restart, `agents restart` of a current step included, recomposes the note for its fresh or resumed leader.

### How an attempt ends

`crates/rimz/src/harness/workflow/engine/agent.rs::prepare_end` observes once, stamps the clock after the observation, and updates the hold before it decides an end, in this precedence order:

| Condition | Outcome or effect | Route |
| --- | --- | --- |
| Unfired run deadline at or before now | `canceled` | raises `deadline`, once |
| Agent step, no open hold, `started_at + timeout + asked_ms` at or before now | `timeout` | raises `timeout` |
| Supervised run `completed` | `ended` | `then` |
| `failed`, `verify_failed`, or `timed_out` | `died` | raises `died` |
| `canceled` | `canceled` | raises `died` |
| Run `pending` or `running`, a blocking ask, and an explicit `on.asked` (turn or session, whatever else is observed) | `asked` | `on.asked` |
| Run `pending` or `running`, the leader's own errored turn (`Observed::errored`), and an explicit `on.errored` | `errored` | `on.errored` |
| Run `pending` or `running` with any other observed provider limit, error, interrupt, or budget park | Keep the attempt live, holding it on that stop (`LiveAttempt::held_on`) | None |
| No supervised run, and the launch host is dead | `canceled` | raises `died` |
| Session with a `rest-check`, new eligible leader turn end | Hold the session aside and evaluate its first rest check | The saved verdict decides keep or close |
| Otherwise | Keep the attempt live, and persist only a changed hold | None |

The Route column is one rule, `Definition::ending_cause` under `Definition::target`. Settlement (`Transition::settle_step`) and the views both read it.

The terminal-status and explicit-ask mapping is one pure function, `crates/rimz/src/harness/workflow/engine/agent.rs::observed_outcome`, which takes no lifecycle, so a session ends on a blocking ask only when the step routes `on.asked`, and on an errored turn only when it routes `on.errored`, as a turn does. Timeout and deadline stay outside it. Live provider limits and overloads never end an attempt. The errored turn is read from the leader's own row and its supervised run's `stopped_turn` (`crates/rimz/src/harness/workflow/observe.rs::observe`), never from the merged stop, so a companion's error or an earlier limit cannot end the leader's attempt. A routed ask wins over a routed errored turn. A pending ask therefore beats the step timeout even with `on.asked`, but not the deadline or cancel. A completed run still ends `ended` even when an ask is still visible. `prev_attempt.asked` is set only for an `asked` ending, and `prev_attempt.errored` only for an `errored` one. An `errored` route parks with `ParkReason::Asked` if it ever reaches `@user`, which admission refuses on every route.

A route through `then` forwards the run's routed input unchanged, with the current key: the attempt's outcome and evidence go to `prev_attempt`, never into the input. A launch refusal other than the budget gate's ([Launch retry](#launch-retry)) ends the attempt `died`, with the refusal text as the attempt's reason. A workflow-owned supervised run never ends `budget_exceeded`: `harness::run::budget_exceeded` leaves it open. A queued asynchronous question does not end an attempt.

#### What the host observes

The host reads the leader's run record and the room snapshot with budget parks projected, and `harness::workflow::observe` decides from them.

- It scans the leader, then its live launched descendants depth first, past an ended child and everything under it.
- It reads each agent's first actionable ask, not one by ask id, until it finds a blocking one, and never reads an agent whose permission is still under automatic review. That permission's hold starts when its review ends.
- Limits come from typed turn-error classes, never from provider text.
- The earliest stop wins, a tie keeps the one scanned first, and an interrupt stands only while nothing else stops the attempt.
- Companion trees never control an attempt's end.

Workflow-owned supervised runs record an errored `TurnEnded` or `TurnInterrupted` as an optional `stopped_turn = {at, disposition: failed | canceled}` rather than a terminal status. They stay Running and clear any inherited owed-wake `parked_at`, and the next `TurnStarted`, `ToolUsed`, `AwaitingInput`, or clean end clears the evidence. Non-workflow runs keep their terminal-disposition table. Session runs record the same evidence across turns. `Observed.stopped` combines this with displayed turn-error classes and the card's budget-park timestamp, as `limit`, `error`, `interrupt`, or `budget`. Failed and interrupted turns do not enter a rest check. A turn run's provider exit still becomes Failed and raises `died`.

#### The ask hold

For turn and session attempts, `LiveAttempt.asking` stores `{since, text, kind, handle}` while a blocking ask is observed. `held_since` opens one shared hold at the earliest ask, limit, error, or budget-park evidence, clamped to `[started_at, now]`, and `asked_ms` stores closed held time as u64 milliseconds. Overlapping evidence keeps the hold open and is credited once, when all pausing evidence clears. Silence and interrupts do not hold the timeout.

- An ask's visibility normally begins at its open time. A Codex Auto permission is hidden during automatic review and becomes visible 60 seconds after arrival, unless the rollout proves the asking Code Mode cell finished and closes the ask, so review time does not pause the attempt timeout ([waiting and asks](../agents/model.md#waiting-and-asks)).
- Further asks keep their interval's start and replace its text, kind, and handle.
- A stable hold writes nothing, and a hold-only write journals and accounts nothing.
- Already-ending attempts do not update the hold, and staging any end clears it.
- Two holds without an intervening observation merge into one pause.
- A step already overdue when the hold opened times out after it clears.

Optional fields default when absent and are omitted when empty or zero. Records without pause fields start with no pause.

Leader lifecycle hooks wake the run. Subagents have no workflow pin and are sampled by the roughly one-minute tick, so a subagent's answer may stay credited and displayed until the next tick.

#### A session's end

A session records live ask holds and never ends on a live provider limit. Its `timeout` ends it as `timeout` once due. Turn ends, interruptions, session-end hooks, and RimZ budget parks keep its session run open, and a `rest-check` adds the check-driven close below. A budget park still interrupts the provider and stamps its ledger, and a budget-gate refusal before launch waits `budget` with no attempt ([Launch retry](#launch-retry)).

A provider exit completes a session as `ended` only when it had ended a turn and was at rest (`turn_started_at` set and `idle_stop::rests` holds, `crates/rimz/src/harness/workflow/mod.rs::exited_at_rest`). Any other exit, an unregistered session's included, reads `died`. That reading uses the row's turn stamps and rest states only: the rest check's debt gate (an owed message, a running child) and its pending-ingress gate in `leader_turn` are left out, so a session that exits at a clean rest while it owes a message or a child's result settles `ended`. A missing snapshot row or an unreadable snapshot also settles `ended`. The run deadline beats a completed observation and raises `deadline` with outcome `canceled`. Cancel, restart, reset, and rebirth also cancel sessions.

The row read is the agent's holder: a run that still names a session a restart or molt replaced finds the holder through the retained rows. A restart or molt hands the leader's open session run to the replacement (`open_session_run` in `crates/rimz/src/cli/agents_cmd/restart.rs`), as a restarted child's run is handed over ([subagents.md](./subagents.md)), so the replaced session's exit settles nothing, before or after the replacement binds, mid-turn or at rest. `crates/rimz/src/harness/run.rs::settle_session_exit` rechecks the stamp under the lock. A replacement whose bind is refused never takes the run, and one that fails after its bind fails it.

### Rest checks: judging an open session

A session's `rest-check` is a nonempty list of check names (`Node::rest_check`, empty for a turn, a check, or a session without one). The rules:

- Entry at a rest follows the first name, and membership keeps a held session open across a route between listed checks.
- The list carries no adjacency rule: routes alone decide where the run goes next.
- From a rest check, a route to the session's own node sends the answer back into the session and counts a visit, `@wait` keeps waiting, a route to another listed check continues the rest there, and any other route closes the session and goes there. Failure routes from a rest check follow the same rule.
- The session's `then` is where the run goes when the agent exits on its own.
- A check reached without a held session keeps ordinary routing.

#### Holding the session

An eligible leader turn end moves the agent attempt into `RunRecord.held = {attempt, entered_at, verdict?}` and enters the first rest check, with no visit and no input or key change. `RunRecord.node` then names the check, while the views still show the session's step. The held attempt is a session agent attempt with a run, a `judged` turn identity, and no ending, and the slot is empty or holds that check's script or judge attempt. The ordinary check engine owns windows, sequences, publication, timeouts, and CAS while the session is aside, with dead-host reclaim for a script and the agent-attempt lifecycle for a judge ([Judges](#judges)). Routing saves `{node, entered_at, at, target, object, reason}` in `held.verdict` instead of applying it, where `at` is the evaluation's journal instant. A held session never survives restoration, staging an ending, parking, or terminal settlement.

#### Which turn ends are eligible

The turn identity is `{agent_id, started_at, ended_at}` from the leader's projected turn stamps, with `ended_at >= started_at` (successful compaction at rest can supply equal stamps). An interrupted turn can have a closed identity but is not eligible while stopped. The engine requests `Observation::LeaderTurn` only when it prepares an attempt whose step has a `rest-check` and no staged ending, or observes its held session. Turn attempts, sessions without a rest check, cancel, and post-stop cost observations use `Observation::Attempt` without leader-turn reads.

The host computes rest fresh under the workspace lock: `idle_stop::resting`, no owed wake, no queued message, and no other open supervised run apart from exactly this session's run id. The engine also requires no blocking ask and no non-clean stop, descendants included.

Pending ingress blocks rest only when its `RIMZ_WORKFLOW_RUN` names this workflow run and its `RIMZ_RUN_ID` names this leader's supervised run, so detached former leaders and companion frames do not block the current leader. The one frame exempt is the one named by this advance's `--woken-by` ingress id. The waking frame's apply has committed before it spawns the advance, but the drainer publishes its applied cursor only after the apply exits, so without the exemption the advance would wait on its own wake. The exemption lasts for this advance's lifetime, including the fresh observation after evaluation, and every other pending frame naming this leader's run still blocks.

`LiveAttempt.judged` consumes the identity before evaluation, so restoring the session cannot begin another initial check for the same end, and a held wait re-samples at the check instead.

#### Held preflight

Before any other preparation, a held preflight observes the session and folds its hold, then checks the deadline and provider exit or budget-exceeded status before it considers a verdict.

A same-step verdict (a push-back) closes any wait, restores the session, and applies its input and key. It checks the visit bound before it bumps once and stamps `LiveAttempt.visit`. An answer past the bound raises `visits` at the step and closes the session with a Lifecycle ending, so a `keep = true` agent is detached. Otherwise it publishes `LiveAttempt.push = {message, queued: false}` and `TurnEnd {action: pushed, visit, message}` under the workspace lock, then calls `AttemptHost::push` off-lock. The host composes a System/Done synthetic message and enqueues it atomically only when the id is in neither the live queue nor any retained history, and delivery uses the ordinary message pipeline. A failed or interrupted push retries the same id without recounting. The host's enqueue acknowledgment is persisted only when the sequence and message id still match. Restoration keeps the session's sequence, run, and step entry time, clears `seen`, and rewrites the context to the step view.

A `@wait` answer leaves the session held at the check, clears its saved verdict, merges an ordinary rechecking wait, and writes `TurnEnd {kept}` without a visit. A new wait's `first` and every merge's `last` use the answer's `at`. Subscribed signals and the minute tick re-sample the held wait. When the leader no longer rests on its judged turn, preflight closes the wait, abandons any check in flight, and restores the session with `TurnEnd {discarded, reason: "leader turn moved on"}`, and the next clean rest is judged.

A move to another listed check requires the same eligible judged turn as a push or a close. It closes the source wait, clears the saved verdict, updates the node and entry, and evaluates the destination in the same advance. The key stays. The input stays too, unless the verdict object is a failure (`outcome: failed`), which replaces it (`Transition::hand_on`). It writes `TurnEnd {action: kept, check: source, target: destination}`, and history, hop, and assist readers project the route from that row. Every decision names its actual check, a discard or interruption before a verdict exists included.

#### Closing the session

A closing verdict needs a fresh locked observation after evaluation:

- A different or missing turn tuple discards the verdict and keeps the consumed marker. The advance may then check a newer eligible end.
- An identical tuple that is no longer eligible defers the saved verdict to a later advance, without paying for the check again.
- An identical eligible tuple restores the session and stages `Ended/Lifecycle` with `Ending.route = verdict`, then uses the ordinary off-lock stop or detach.

Settlement journals the ended attempt and the closed turn end, restores the check's node and entry time, and applies its route exactly once instead of following `then`. A step destination launches normally, and another check sees the ended session in `prev_attempt`. Without a held session, the route applies at once, including after `resume --at` the check. A saved verdict resolves before the ordinary check timeout, even with an open wait.

Provider exit and the deadline abandon any live check, restore the session, record an interrupted turn end, and stage the ordinary session ending. Cancellation does the same before its stop. A cancel that replaces an already-staged verdict close records an interruption and removes `Ending.route`, so late settlement cannot also record a close. Check timeouts still start from the hold's check entry, while the deadline and the session's own timeout belong to held preflight, and a session timeout abandons the check the same way. Judge backoff never suppresses the scheduler's paced session observations.

#### Turn-end records

Each judged end or held-wait answer has a `turn_end` decision (`kept`, `pushed`, `closed`, `discarded`, or `interrupted`) carrying the session sequence, step, turn identity, check, and an optional verdict outcome and target or an explanatory reason. [Accounting](#accounting) owns which of them book an assist.

`context.json` has `live_attempt = {step, seq, visit, visits, key, started_at, handle, run_id, claim_path, head_start, turn_end}` while a session with a `rest-check` has no staged ending, held or in the slot. Its turn end is `judged` or null, and it has no outcome, ending timestamp, or ending commit. `prev_attempt` stays the last ended attempt. Check publications keep the check's top-level sequence without step visit fields. A push-back or discard restores the step view, a held wait keeps the check view, and a close leaves the next publication to replace it.

#### Accepted limits

- A turn that starts after the final per-run ingress read but before the off-lock stop can be cut off, since stopping has no provider or input coordination.
- Fast turns can coalesce into the latest eligible rest, rather than a lossless replay.
- A long check window or judge-limit wait can spend a check whose later verdict is discarded.
- Publication appends the journal, writes the record, then appends assists, so a crash can duplicate a journal row or miss an assist, as for other transitions.
- A held forge reading can be one probe old, back to the session's start rather than the judged turn, so a same-SHA CI rerun or a PR reopen inside that turn can read as its earlier state. CI on a new head is still fenced by the PR head. A fact that changes after the leader's last turn end is sampled at the next eligible end, or while held at `@wait` when a subscribed signal or the minute tick re-samples the check.

### Stopping an attempt

Every end publishes `ending` and saves the observation into `prev_attempt` under the lock before anything stops, which keeps the asks and limits the stop would retire. Then:

1. Off-lock, the host stops every pinned layout agent and its tree. `harness::workflow::stop_cohort` picks them: every unended row with the attempt pin, even when the leader row is absent or ended, and once a pin is cleared the detached leader and its launch group. An ended child's subtree is never reached. The host falls back to the supervised stop only when neither inventory remains.
2. It observes cost right after the stop, then samples HEAD.
3. It locks again, checks the sequence and the stopper's identity, journals the attempt, and routes.

No successor launches before the stop or detach returns. Attempt cost comes from the leader run after the stop or detach. Companion spend stays on cards and in stats, and unstamped spend stays null.

For `keep = true`, `StepLaunch.keep` disables the leader wrapper's completion cleanup, and a Lifecycle ending whose outcome is `ended`, `asked`, or `errored` calls `AttemptHost::detach` instead of stop (`crates/rimz/src/harness/workflow/engine/agent.rs::detaches`). A `died` or `canceled` attempt stops the layout like a forced ending. A push-back past `max-visits` ends as `ended` and detaches like any other natural ending. Detach cancels a nonterminal supervised run and clears each matching leader and companion pin that `harness::workflow::detach_cohort` selects, through `crates/rimz/src/store/writer.rs::Store::clear_workflow_pin`, and stops nothing. Its `agent.workflow_cleared` event compares the complete expected `{run, seq, role}` again in the reducer, and a mismatch keeps its pin. The event changes no card field. Cleared cards are ordinary recovery candidates. `agents resume` refuses a current pin with the workflow-restart fix, clears a former pin only after preflight, and keeps it with a warning when the workflow record cannot be read.

Settlement rechecks a forced ending after an off-lock detach, so a cancel that replaced the Lifecycle ending still stops the layout before settling. Stop can recover already-detached, unpinned cells from the leader's original launch group, and a card pinned to a different attempt is never selected by that fallback.

- A later advance leaves a live stopper alone and reclaims a dead one, merging the staged child evidence with the new stop inventory.
- A cancel during launch leaves the live launch owner to publish and stop the run it gets back.
- When the launcher is dead, the stopper asks the host for the unpublished supervised run by its workflow id and attempt sequence (`AttemptHost::orphan`, which both hosts answer through `harness::workflow::orphan`), and stops it before settlement.
- Live launched descendants are stopped first, and they and companion cells are recorded under `children.stopped`. Independently launched peers and team cohorts survive under `children.live`, with their board stage when available.

Stops use the shared cascade and add no workflow-specific lifecycle events. The next visit's occupancy read skips a stopped attempt whose owner has exited, even while its cached row remains.

## Judges

A judge evaluation is an agent attempt: `engine::prepare` gives it a fresh `seq` and a session-lifecycle attempt of kind `judge`, and it launches through the step's launch seam (`crates/rimz/src/harness/workflow/engine/agent.rs::step_launch`) with the Judge role and the check's profile. It takes no keep, resume, overrides, takeover, or eviction, counts no visit, and writes no step-launch assist. The production host launches a Judge-role pin in automatic permission mode unless its profile sets another (`LaunchOverrides.default_mode`). Folder trust and the adapter's minimum version are the launch's own, and a launch failure fails the judge `died` at once, except a trust refusal the user can clear in place, which waits `trust` as for a step ([Launch retry](#launch-retry)). A budget that would refuse the launch holds the judge as the `budget` wait instead, through `prepare`'s read or the launcher's `NotLaunched::Budget`, as for a step ([Launch retry](#launch-retry)). A launch the host reports as having started nothing (`NotLaunched::Unstarted`) is a failed eviction, and a judge never evicts, so the step's one `retry` wait never reaches a judge.

### The answer channel

`crates/rimz/src/harness/workflow/engine/agent.rs::compose_judge` composes the prompt: the check's prompt file and the `submit` contract (`crates/rimz/src/harness/workflow/brief.rs::judge_prompt`). The judge answers with `rimz workflow submit`, which resolves the calling agent's workflow pin, refuses any caller but the current Judge-role attempt, and calls `crates/rimz/src/harness/workflow/engine.rs::submit`. That normalizes the answer as a script's stdout is normalized (`crates/rimz/src/harness/workflow/state.rs::normalize_object`), resolves `--recheck-at` from now in the `timezone` setting, and stores it on `attempt.answer` under the workspace lock, refusing a second answer and any answer once the judge's `crates/rimz/src/harness/workflow/inspect.rs::timeout_at` has passed, since the next advance reads an answer before the timeout. Nothing reads the judge's last message.

The next advance reads the answer in `prepare_end`, ends the attempt through the stop protocol, and `Transition::settle_judge` settles it through `Transition::finish_check`, the cost patched onto the evaluation. Settlement reads the definition again after the off-lock stop, falling back to the one the advance read when that read fails, as a script check does after its child (`crates/rimz/src/harness/workflow/engine/agent.rs::finish`). It runs `fit::misfit` on that definition first, so an edit during the stop that the run no longer fits ends it as a misfit. An answer that a definition edit made unroutable before settlement, the stop included, raises `invalid`. A judge marked `woken` whose answer settles into a wait clears the wait's and the record's `recheck_at` and evaluates again with a fresh judge. A held session's rest-check judge leaves that to held preflight. The deadline wins a tie with an answer.

### Rest, holds, and endings

| Ending | Result |
| --- | --- |
| First clean rest (`rest::eligible`) without an answer | One reminder (`brief::judge_reminder`), recorded as `attempt.reminded`. Later rests wait for the timeout. |
| A question, a provider limit, or a budget park | The shared hold of an agent step records `asking`, `held_since`, and `held_on` for the views, but the timeout keeps running: the judge answers in time or ends as `timeout`. A question never ends a judge, and validation refuses `on.asked` on one as a dead route. |
| The judge's own errored turn (`Observed::errored`) | `errored` at once, through the failure chain. A timeout seen at the same advance wins (`prepare_end` tests the timeout before the observed outcome). |
| The session ends without an accepted answer | `died` (`the judge's session ended without an answer`). |
| Its session closed under it (`canceled`) | `Transition::abandon_check`: the next pass evaluates the check afresh. |
| Timeout or deadline | `timeout` or `deadline`, after the stop. A `timeout` that passes while the judge is stopped names the stop in its `reason` and `detail` (`asked`, or `stopped` with `reset_at` beside a limit's known reset when `Observed::limited_by` shows the reset was read from the stopped agent, since `observe` reads the reset of the first limited agent only), built by `state.rs::stopped_evidence` from the observation of the ending advance, since staging clears the hold fields. An ask wins over any other stop, as the hold orders them. |

A judge's ending stages its evidence on its own attempt (`attempt.evidence`, `crates/rimz/src/harness/workflow/state.rs::judge_evidence`) and never replaces `prev_attempt`, which stays the last step's. Its timeout runs from the attempt's start and is not due before the launch (`inspect::timeout_at`), so an `admission.window` or `budget` wait spends none of it, while `held_since` and `asked_ms` never move it. A judge's ending staged by an older RimZ settles by the same rule: a staged `errored` fails it with that cause, and a staged `asked` never reaches it: the run's definition still routes the judge's `on.asked`, which admission refuses, so the run waits at the refused definition, and once an edit removes `on.asked` it ends as a misfit (`engine/fit.rs::misfit`).

An interrupt of a held session abandons the rest-check judge evaluating it: `abandon_check` journals an engine `canceled` evaluation and pushes the judge's run onto `record.abandoned`. The next engine pass and `agent::finish` take that list, persist, and stop each run off the lock (`agent::stop_abandoned`). `workflow restart` and `cancel` stop a live judge through the same stop protocol as a step's agent.

### Isolation

A judge is read-only on the scope checkout and its Git admin and common directories. Scoped isolation overlays them read-only for a Judge-role launch after all reach rebinds, and host isolation enforces the rule only through the prompt. The root view and the required-path rebinds leave `work/` writable, a room home under host `/tmp` included. A judge gets no subagent catalog, is never served the `rimz-subagents` MCP capability (`crates/rimz/src/harness/launch.rs::ExecRequest::mcp_capabilities`), and keeps native delegation.

### Declines

A fresh judge decline in a task-started run appends a `JudgeDecline` assist (`crates/rimz/src/harness/workflow/engine/agent.rs::record_decline`, after `agent::finish` persists): a verdict whose route leads to no step or agent check while the opposite verdict's route leads to one. Only a run that names a task records one. A loop fire's command row compiles no judge, so a fire's own run never reaches it.

## Wakes

A wake only spawns an `advance`, which reads the record again. `run`, `resume`, and `restart` spawn one, as does `extend` on an active run, and `rimz workflow advance <id>` is the manual form. There is no signal replay: a lost wake delays progress until the next tick.

### The signal helper

Every signal routed through the scheduler's shared firing entry spawns one room helper when the room has an active run (`crates/rimz/src/harness/schedule/signal.rs::wake_workflows`). A lifecycle signal spawns one only for an Active run whose current check's subscriptions deliver it or whose live session's step subscribes to its family (`engine::hears_lifecycle`, which also reads those runs' definitions and skips a run whose definition does not read). A check's `wake` can name only the `workflow` family among the lifecycle signals, since validation refuses an agent or team lifecycle selector there. Neither the spawn nor the helper wakes a run on a `workflow.*` signal whose payload `run` is its own id (`engine::names_run`), so a run's own move never re-wakes it. That path only reads records and spawns, carrying the whole signal as JSON in the helper's argv.

The helper (`engine::wake`) takes the workspace lock per run, reloads each active run and its definition, and matches the current check's subscriptions and the live session's step subscriptions, through the same delivery matcher as loop triggers. A sibling skip does not wake. A run where neither matches gets no write, journal entry, or spawn. A run where either matches is admitted before anything else ([Engine reads](#engine-reads)). A run that fails to load or is refused warns without blocking the others.

On a check match the helper clears the record's and the wait's `recheck_at`, journals the signal name, and marks a live evaluation and a held session `woken`. Publishing a new held check request clears the session flag before reading its inputs. When a wake arrives during that check, or before its saved `@wait` merges, the merge clears the new backoff and re-evaluates at once, once, bypassing the hot-loop guard for that wake. Ordinary evaluation settlement does the same for a woken `@wait`, and so does a judge's settlement after its stop. Wakes during the retry can ask for another pass, and without one the ordinary guard applies. The helper spawns one `advance` per matched run, so one slow script cannot hold back another run.

A step subscription forwards to the leader of `RunRecord::session_attempt` (`engine::subscribers`) only while that attempt is not a turn, has no staged `ending`, and has a supervised run. The held session counts, so a check and its held step can both take one signal.

1. The helper mints the message id under the lock and records it in a `signal` journal row keyed to the session's step and seq.
2. It queues the message off-lock through `AttemptHost::forward`. The production host queues it once under that id, as `Type: SIGNAL` from `@rimz` (`HarnessNotice::Signal`), automated, at the done gate, with `about_run` naming the attempt's run, then attempts delivery at once as `Host::push` does.
3. A run that ends before delivery withdraws the message (`crates/rimz/src/message/premise.rs::broken`), so subscriptions retire with the attempt on every path that ends its run.

The text opens with `signal_subject` and the details the loop's SIGNAL delivery prints, then the workflow, step, and run id. Every binding that delivers forwards its own message with its own `prompt`, and bindings with the same selector and `match` coalesce into the first. A step-only match writes only that journal row: no record write, assist, or advance. A failed queue warns and keeps the row. A check match writes the row in the same transition as its `wake` row.

### Subscriptions

A check's subscriptions come from its `wake` list, or, for a condition or conditional sleep, from its terms (`Check::subscriptions`). A plain sleep has none.

| Condition term | Derived selector |
| --- | --- |
| Checkout `ci` | `ci.*` |
| Checkout `pr`, `pr.queue`, `pr.mergeable`, or `pr.review` | `pr.*` |
| Stage | `team.stage` |
| Stage with a branch selector | `team.stage` with `board = "*"` |
| Forge branch selector, or `window.*` | nothing |

Negated terms derive the same selectors.

A step's subscriptions come from its `subscribe` list (`Step::subscriptions`, through `Wake::step_subscription`). Validation refuses `subscribe` on a turn agent step, and the `command` and `message` tables have no such field. Unlike a check's wake, a step subscription may name a team lifecycle selector, and an `agent` selector when its `match` names another agent by a non-wildcard `handle` or `session`. An `agent` selector without one refuses (`SpecErr::SelfSignal`).

A predictable name can still be the leader's own (a team role's handle), so `engine::subscribers` and `engine::hears_lifecycle` also skip a lifecycle signal whose payload `handle`, compared without its channel, or `session` is the leader's (`engine::from_leader`): a leader never hears its own lifecycle. The leader's session is read only when the handle does not match and the signal names a session: through `AttemptHost::leader_session` in `subscribers`, and through a reader `schedule::signal::wake_workflows` supplies in `hears_lifecycle`, which runs where the signal fires, with no attempt host.

At match time, for checks and steps alike (`engine::delivers`), selectors are narrowed to the node's checkout. Nothing is persisted.

- `ci` and `pr` selectors add the checkout's `path`, unless `path` or `branch` is explicit.
- `team.stage` adds the board `crates/rimz/src/harness/scratch.rs::durable_board_path` resolves for that checkout, unless `team`, `instance`, or `board` is explicit. The branch form's explicit `board = "*"` keeps it room-wide, since its board lives in another checkout.
- Another team signal, which carries no board, adds the `instance` of its team's cohort in that checkout: `<team>#<channel>`, the routing name of that checkout (`crates/rimz/src/worktree/own_lane.rs::own_lane`), or the team's own name at the project root.
- Other families stay room-wide.

A check's checkout is `scope.path`. A step's is the checkout its leader launched in (`engine::destination`), so a step with its own `worktree` hears that worktree's CI and board. A checkout relink therefore follows its new archive, and project scope matches the project root. A check with no subscriptions runs only on its recheck time and the tick. A matched wake clears the run's recheck time and marks its current attempts woken (`engine::wake`): a waiting check evaluates at once, and a judge or a held session's rest check that the wake reached while it ran evaluates again rather than waiting out its answer's recheck time.

### Supervised-run edges

An agent step's supervised run wakes its own workflow directly, through `engine::wake_run`. Every newly terminal run write through the harness wakes the run named in its `workflow` field after releasing the workspace lock: provider exits without hooks, wrapper failures, timeouts, and rebirth cancellation included ([scripting.md](./scripting.md#the-record)). Workflow budget parks never settle a run. A hook-ended run that is not a peer turn settles its session spend before the wake, so the advance reads the stamped cost. Non-terminal lifecycle and turn-error frames carrying `RIMZ_WORKFLOW_RUN` wake only that run. An advance spawned inside a frame's apply names that frame's ingress id on argv (`--woken-by`), so its own still-pending frame cannot defer rest, and advances outside an apply carry no waking id.

Room teardown cancels supervised runs without waking workflows, and the next advance settles those attempts. Rebirth excludes every workflow-pinned cell from recovery and cancels nonterminal workflow runs even outside the roster, unless a live provider or wrapper still serves them, then wakes each workflow to decide again. A stop never closes a supervised run's pre-rebirth pane id in the new session. A descendant's frames carry no workflow pin, so its asks and limits are sampled at the next step edge or tick, within about a minute.

### Run signals

A run fires its `workflow.*` signals from its committed records, never from the transition that wrote them. `engine::Commit` replaces the bare workspace lock at every engine site that has a host: `engine::persist` reads the record on disk before it writes and queues the before and after pair when the node, the state, or `RunRecord::needs_you` differ, and dropping the guard releases the lock first and then hands each pair to `AttemptHost::committed`. Firing appends to the store, which takes the same workspace lock, so a fire under the guard would deadlock. The production host (`crates/rimz/src/launcher/workflow_host.rs`) maps each pair through `crates/rimz/src/harness/schedule/signal/workflow.rs::run_signals` and fires each signal in process; the engine itself imports nothing from `harness::schedule`. The mapping fires `workflow.moved` for a changed node on a run that goes on, `workflow.ended` (with `workflow.failed` on a failure) on the write that makes the run terminal, and `workflow.waiting` when `needs_you` turns true, the one reading `workflow list` and `workflow wait` also call. An inline run (`RunOrigin::Inline`, a shell loop task's command) maps to nothing: its ending would otherwise fire a task subscribed to `workflow.ended` once per run of its own command, without end.

Lock sites without a host hand nothing on: the run's creation, `workflow submit`, a task reassignment, and the script spawn write. `store::workflow::end_stranded` is the one terminal writer outside the engine: it returns the header it read before the ending, and `crates/rimz/src/cli/workflow/reconcile.rs::settle` fires `stranded_signals` from it after dropping the lock, when the header names a project root. A loop shell task's own inline run fires nothing there, as its engine edges fire nothing; a header with no `origin` predates inline runs, so it reads as a deployed run and fires. There is no replay: a process that dies between the write and the fire loses the signal.

## The tick

`crates/rimz/src/harness/schedule/fire.rs::fire_due_tasks` runs every scheduler tick, from the room host or the external timer. After it fires due loop tasks, it calls `tick_workflows` for the root, then the loop backlog tick. `tick_workflows` reads the room's live directory (`crates/rimz/src/store/workflow.rs::live`) and spawns `rimz --root <root> workflow advance <id>` for each due run this RimZ continues.

An active run is due when:

- it has no `recheck_at`, or its `recheck_at` or its unfired deadline has passed
- it holds a condition case
- a session is held at its rest check

Parked runs are never spawned. The engine decides everything else (bounds, window, occupancy, capacity, a live host). The tick reads `recheck_at` so that early advances do not inflate an ordinary wait's count, and it never clears backoff the way the signal helper does.

A terminal run is due only while it still owes its loop task, its starting agent or its starting `runs` step a report ([Loop tasks](#loop-tasks)), and that advance's command only collects it: an owed run keeps its live home and stays due until the collector marks its report sent. The terminal transition keeps the run's last spawn stamp, so its first report advance waits out the same 60 seconds from that spawn. After that it is due again once the time since the last spawn reaches the time the run had been finished at that spawn, clamped to between 60 s and one hour (`crates/rimz/src/harness/schedule/fire.rs::tick_pace`), so the gap between failed collections doubles.

Spawns are paced by the runtime cache `lanes/workflow-tick.json`, a map of run id to last spawn: at most one spawn per run per 60 seconds (`WORKFLOW_TICK_SECS`), except that a `recheck_at` or deadline that passed since the last spawn fires at once. A plain sleep stores its end as `recheck_at`, so it gets no intermediate spawn. A conditional sleep stores no end, so it keeps sampling each minute, during a hold too. The tick writes the cache before spawning and drops runs that are neither active nor owed a report. A read failure counts as an empty cache, and a write failure warns and skips that tick's spawns. `fire.rs` is in the sidebar import graph, so it writes only that cache and spawns, and the invariant grep keeps it off the writer and the wake socket. Under the systemd timer the spawned advance leaves the oneshot service's cgroup, as a loop run does.

When any live home awaits [the workflow pass](#the-workflow-pass) (`RunClass::awaits_pass`: a terminal run this RimZ reads that owes no report, a terminal record this RimZ cannot decode, a run this RimZ cannot continue, or a record that holds no JSON object), the tick also spawns `rimz --root <root> workflow reconcile`, at most once per room per 60 seconds, paced by the runtime cache `lanes/workflow-reconcile.json`, whose stamp it publishes before the spawn.

The cadence a workflow author sees: an ordinary check's `@wait` without `recheck_at` is re-evaluated about every minute, so a check that wants a slower pace sets `recheck_at`. A held rest-check `@wait` follows the same cadence and subscribed signal wakes while the leader rests on its judged turn. A live step is observed every tick, so a descendant's ask or provider limit is noticed within a minute.

The external timer's roots (`crates/rimz/src/cli/loop_timer.rs::task_roots`) include, beside the rooms of task rows and open fire marks, every room whose live directory holds a nonterminal run this RimZ continues, a terminal one that owes a report, or a home that awaits the workflow pass. So a manual run in a room with no loop task and no sidebar still ticks, and a stranded run is ended there too.

### The workflow pass

`rimz --root <root> workflow reconcile` (hidden, `crates/rimz/src/cli/workflow/reconcile.rs::run`) settles every home of the room's live directory, one at a time, each under its own workspace-lock holds, so one failing home never stops the rest and no hold spans the room. One pass runs per room at a time, through the runtime lock `workflow-reconcile.lock`, and a second exits at once. Each home takes its class from `store::workflow::classify`, and `crates/rimz/src/cli/workflow/reconcile.rs::settle` acts on it:

| Home | Action |
| --- | --- |
| Stranded | `store::workflow::end_stranded` patches the record in place under the lock: `state` `failed`, a first `terminal_at`, `outcome.ending` `failed` with the reason `this RimZ cannot continue the run: <cause>; it reads only record version 19`, `outcome.acted` kept or `false`, `loop_report` owed when the record names a task, and `stranded: true`. Every other member and the version stay. Then, off the lock, its report is made from the header, its agents are released (`harness::run::release_workflow`), and the home moves with a header-only `history.txt`. |
| `NotObject` | Agents released, then the home moves unmodified, with no history. |
| `Current`, terminal, owing a report | Nothing: its paced advance collects it ([The tick](#the-tick)). |
| `Ended`: a terminal record that is not version 19, or a version 19 record that does not decode | A report from the header when it owes one, `mark_header_report_sent`, then a header-only `history.txt` and the move. |
| `Current`, terminal, owing nothing | The history and the move. |
| `Current` nonterminal, `Newer` (any state), `Recordless` | Nothing. |

A report made from the header (`run_log::report_header`) is strike-neutral: it appends the run's row, forgets the launch memory of a run that did not end `done`, and drains the task's waiting triggers, but never strikes, resets the strikes, ends a quiet streak, or settles a checkout. A record that `end_stranded` marked `stranded` takes the same neutral path even when it decodes.

Each step waits for the one before it to land:

- A report that does not land (the history cannot be appended) warns and leaves the run owed and live, and the next pass retries it.
- The release runs only after the report landed. A release that fails (a supervised-run record that does not decode) warns and leaves the run live, its report already sent and its loop freed, and the next pass retries the release.
- The move runs only after the release succeeded, in the same pass, so the notice names the agents that release freed. When that move then fails, the next pass finds no pin left, and its notice names none of them.

A run is announced by the pass whose move of its home succeeds, with its cause read again from the record (`RunHeader::stranded`), so a run whose report or move fails is announced by a later pass, and each run once. A crash between the move and the notice leaves that run unannounced, since no later pass sees a finished home. A pass that moved any such run writes one `workflow_runs_ended` diagnostic ([diagnostics.md](../diagnostics.md)) and sends one `workflow_ended` notice naming no agent, through the notification handlers and a sidebar toast: `N workflow runs this RimZ cannot continue were ended as failed: <count and cause, ...>. N agents were left running. Inspect with <command>`. The sentence on agents appears only when the pass released one, and the command is `loop list` when every ended run came from a loop task, else `workflow show <id>`. The notice says nothing about loop reports, since a home moves only after its report landed. A pass that moved none sends nothing.

`workflow advance` on a stranded id settles that one run the same way and notifies, as it does for a run an earlier call ended as stranded whose release or move failed, still in the live directory. `workflow cancel` on one records the diagnostic and prints a receipt instead of a notice.

## Loop tasks

A shell task's fire starts a workflow run of the inline graph that `crates/rimz/src/harness/schedule/shell_task.rs::command_definition` builds from the row: one `test` check named `command` that runs the row's argv under its `timeout` ([loops.md § The compiled graph](./loops.md#the-compiled-graph)). A workflow task's fire runs `rimz workflow run <target> --loop-fire-json` in a launch child, which publishes a run of the deployed workflow carrying the fire (its `evidence` included, so the run's commands read the same `RIMZ_LOOP_TRIGGER` and its agent and message steps get the line after the run's `--prompt`), and an agent or team task's fire publishes no run of its own. [loops.md § One fire](./loops.md#one-fire) owns the gates in front of the run.

### What a fire puts on the record

The fire ends in `engine::create` (`crates/rimz/src/harness/schedule/runner/shell.rs::Fire::prepare_shell`) with:

- `task = Some(<name>)`, and `manual` set only for `LoopRunMode::Manual`
- the fire's `fire`, whose `evidence` is the fire's trigger evidence, read once the backlog reconcile has settled which trigger the run serves, and absent when the fire has none. The command reads it as `RIMZ_LOOP_TRIGGER`.
- default start arguments (no `give_up`, `capacity` wait) and no run prompt

The fire takes no start-throttle turn, and an agent its command launches takes its own at that launch.

`Exists` carries the holder's task and scope kind. The fire then follows the task's `overlap` policy against the run (`crates/rimz/src/harness/schedule/runner.rs::Fire::prepare_overlap`): a strike-neutral `overlapped` row naming the run under `skip` or for a manual fire, `deferred` under a retaining policy (`overlapped` when its queue is full), with `replace` also canceling the run. `NoRoom` records `start failed`. [Remembered checkouts](./loops.md#remembered-checkouts) owns the launch entries a fire writes after `create`.

### The report

A task-started run reports its outcome to the task once it ends, and a run an agent started without `--detach` reports to that agent. The scheduler, not the engine, delivers both.

1. `create` marks the record `loop_report = owed` for a task's run, for a run an agent started without `--detach`, and for a run a `runs` step started; every other run, one a command, script or check of another run started included, is created `none`. Every debt gate (the tick, the workflow pass, GC's live-home rule, the agent's owed-wake read) reads the one predicate `RunRecord::owes_report`; the collector below takes a task's run (`owes_loop_report`) down the loop path and an agent starter's run down the starter path, so loop code never collects an agent's report.
2. The record write that makes a run terminal also writes its typed outcome (`RunRecord.outcome`, a `store::workflow::RunOutcome`): the ending (`done`, `tried` for a try that ended done, `failed`, `canceled`, or a capacity give-up with its wait and reason), the summed attempt and evaluation cost, whether the run entered an agent, message or runs step, and a failed run's last reason. The ending comes from the record's state, whether it acted from its visit counts, and the cost and reason from the journal. A journal that cannot be read there does not hold the transition back: the outcome then carries the ending the state names, no cost, and no reason, with a warning.
3. `crates/rimz/src/harness/schedule/run_log.rs::collect_run` collects. Under the workspace lock it reloads the record and, while the run is terminal and owed, appends the report row through `run_log::report_run`, notes its strike when the task's row exists, and then rewrites the mark as `sent`. A run that owes an agent starter instead goes to `run_log/starter.rs::report`: under the lock it stamps the record's `report_message` with a minted message id (`store::workflow::stamp_report_message`, kept on a retry), releases the lock, queues one `WORKFLOW_REPORT` with that id for the starter's live row (`Store::queue_message_once`, which takes an id once across the queue and history), and relocks to mark the report `sent`. An ended or missing starter is told nothing and the mark is set `sent` all the same. A collection killed between the queue and the mark leaves the run owed, and its retry queues nothing new. The same path serves a record this RimZ does not decode, read from its header, so a stranded run's starter is told it failed. A run a `runs` step started reports to its starting run instead (`run_log/starter.rs::report` hands it to `crates/rimz/src/harness/workflow/engine/runs.rs::join_report`): under the same lock its ending, reason, checkout, branch and notes folder are written onto the starting run's attempt, only while that run is still active at the attempt that reserved the child's id and holds no ending for it, and when its record changed the starting run is woken with an advance, spawned under the lock and waiting for it; then the child's own mark is set `sent`. A second collection, or one that arrives after the starting run left the attempt, changes nothing on it and still sets the mark.

The engine never reports and never rewrites the mark. Collection never reads the definition, so a run whose definition no longer loads is still collected. It reads the journal only for extra detail (a command check's exit and log), and an unreadable journal leaves that detail out. A quiet run (done with `engaged` false) of a task whose row exists instead notes the strike reset, adds itself to the task's quiet summary on its task record, and removes its home with no row and no mark ([quiet runs](./loops.md#run-reports-and-quiet-runs)).

Who collects: `rimz workflow advance`, `cancel`, and `restart` (which `rimz agents restart` of a current step reaches) collect the run after the engine returns, whether it returned `Ok` or an error. `rimz loop run` collects every owed finished run of its task (`collect_task_runs`) before it reads strikes, arming, and launch memory. `tick_workflows` treats an owed run as due, paced by its retry gap ([The tick](#the-tick)), and `advance` on a terminal run does no engine work, so its command's collection is the retry.

The mark waits for both the strike and the row. The strike, which closes the fire's mark in the same write, comes first, so a reader that sees the row sees the checkout settled. A failed strike returns before the append, and a failed append leaves the strike noted, each with one warning naming the run, and the run stays owed; the retry's strike moves nothing a second time, since a fire id counts once. A record without an outcome, or a crash before the mark, leaves the run owed the same way. A failed collection never fails the command's transition, which is already durable. So the report can land twice, and its readers fold it by run id ([strikes](./loops.md#strikes)). A record without the mark reads `none` and never reports.

Collection stops nothing the fire's command launched: those launches carry the fire's id as `loop_fire` and end by their own rules or `rimz loop stop` ([loops.md](./loops.md#stopping-a-run)). Until the outcome row is appended, `rimz loop show` and `loop logs` list a `launched` row read from the run's live record. The row is appended after the strike and the mark, so a run whose strike failed shows that `launched` row until a retry lands both.

### Selection, rename, and removal

A task-started run carries the fire that started it (`RunRecord.loop_fire`, a `FireId` that names its task's id), and `rimz loop list` and `rimz loop show` select live runs by that task id (`RunRecord::started_by`), together with the record's project root. `loop show` and `loop logs` also take finished runs by `RunRecord::ran_for` (the task id, any state), and `loop show` joins the task's fire marks by run id. `RunRecord.task` keeps the name the run fired under, for display. Their RUN targets and `workflow show` commands come from one `crates/rimz/src/cli/workflow_view/resolve.rs::RunTargets` load per command. A `rimz workflow run` run has no task, so it appears in `rimz workflow list` and the sidebar under no task. A `rimz loop fire` run keeps its task and records `manual = true`.

`loop rename` rewrites no run record: it rebinds the task's record, and the task's runs, live and finished, select and report by the task id their fire carries, under the task's current name ([The task record](./loops.md#the-task-record)).

`loop remove` first cancels the task's live runs that have not entered an agent, message or runs step (`engine::unacted_task_runs`, each through a detached `rimz workflow cancel` that removal does not wait for), then runs the same transition with no target task, which clears the mark, so a run that already acted continues independently and a re-added task does not adopt it. "Acted" reads the current visit counts (`engine::entered_act`), and a restart clears its step's count, so a restarted run that has not yet been admitted again counts as unacted and is canceled. A cancel helper that lands after that transition cancels a run that no longer names the task, so the removed task gets no `canceled` row for it. A command run enters neither kind of step, so removal cancels every live run of the task. Redefining a task cancels its unacted runs the same way. Finished records keep the fire they ran under, so a task re-added under that name, which has a new id, does not list them in `loop show`.

## Attention

`crates/rimz/src/sidebar/enrich.rs::project_workflow_and_throttle_state` reads workflow records once per fold, producer and passive reader alike, for group projection and `SidebarSnapshot.throttle_notices`. Active `capacity` waits are its only source. Each needs-a-fix cause carries its oldest reason and held-start count, and self-clearing holds add no notice. Workflow parks produce no attention notice. A workflow failure takes the route its workflow sets, often to an agent, or ends the run: only an agent asks, in its own pane.

The same record scan projects live runs into frame-backed worktree groups, including groups with no pane. Only runs of a named workflow directory (`RunOrigin::Deployed`) take part, Active and Parked alike, and every terminal state drops out. A run of a loop task's inline definition shows in `rimz loop list` instead (`crates/rimz/src/sidebar/enrich/workflows.rs::seeds`). Bounded, stat-gated caches supply the main rail, keyed by the run's definition directory so an edit shows on the next frame, and the parsed journal. Reached names are folded on each pass with the record's held session, sharing `inspect::hops` and `inspect::reached` with the CLI rails. Sessions held at a rest check project their held step rather than the evaluating check, and `RunRecord::session_attempt` and `RunRecord::step_agent` keep CLI and sidebar agent identity aligned ([snapshot contract](../sidebar/state.md#the-fold-spine)). A parked run keeps the strip's `parked` word. This path changes neither scheduler forge demand nor workflow transitions.

## Accounting

`crates/rimz/src/harness/workflow/engine/accounting.rs::transition` runs after `persist` publishes a transition and maps the journal entries it wrote. Accounting keys on evaluation origin, not on outcome words, because a check may legally answer `canceled`, an outcome the engine also writes.

- An explicit `Route` suppresses evaluation and attempt accounting. This also keeps a bound's engine evaluation from counting twice.
- A `turn_end` row suppresses evaluation, route, and attempt accounting, retry and rerun diagnostics included.
- A saved verdict suppresses all engine-origin evaluation accounting and diagnostics.

| Journal entry | Accounting |
| --- | --- |
| Explicit `Route` | One `workflow_route`, unless the record now waits, holds a session, or the transition carries `turn_end`. |
| Check-origin evaluation | One `workflow_route`, unless the record now waits, holds a session, an explicit route accounts for it, or the transition carries `turn_end`. |
| Engine-origin `canceled`, run active | `workflow_rerun` diagnostic, no assist. |
| Run canceled | No assist. |
| Other engine-origin evaluation | One `workflow_route` (a judge's failure included), unless the record holds a session, an explicit route accounts for it, or the transition carries `turn_end`. |
| Attempt end | One `workflow_route`, unless the run was canceled, an explicit route accounts for it, or the transition carries `turn_end`. |
| `turn_end` | One `workflow_route` at the check for a push-back, a close, or a kept move between listed checks. None for a kept `@wait`, a discard, or an interrupt. |
| Closed `Wait` | One `workflow_route` to `@wait` at the wait's own node and the summary's timestamp, with `count` set to the final merged count, even on cancel. |
| Park or terminal | `workflow_park` or `workflow_end`. |
| Signal `Wake` or forwarded `signal` | None. |

Held evaluations keep ordinary rerun diagnostics without route assists unless a verdict is saved, and then engine-origin evaluations book nothing and raise no `WorkflowRerun`. No writer raises `WorkflowRetry`; the variant stays so older diagnostic records still read.

An open wait has no route assist yet: `list` and `show` carry its live state, and the sidebar raises notices only for holds that need a fix. Closing a window, budget, trust, takeover, occupied, or capacity wait follows the same summary rule. Route records outside waits omit `count`.

`crates/rimz/src/harness/workflow/engine/accounting.rs::step` appends `workflow_step` after an agent step's supervised run or a command step's child is published, and once per message attempt after the host call, with the target handle when one agent took it. A command step has a null handle and supervised run id. A task's run gets its `workflow_start` from the engine once it first starts an agent, a judge, or a message, or at a terminal transition other than done before that, and a quiet run gets none. `rimz stats` counts the five workflow assist kinds (start, step, route, park, end) each in its own rollup field (`workflow_starts` and the rest), and a `JudgeDecline` in `judge_declines`. Diagnostics are rate-limited forensics that `rimz stats` never reads.
