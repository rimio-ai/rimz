# Supervised runs

> A supervised run until the work ends: the durable record, the completion fold, the park on an owed wake, the wait and its wake socket, verification and retry, the output projections, and pane reclamation. [fleet.md](./fleet.md) is the map for this area and owns the launch machinery and the exec wrapper a run rides on, and [subagents.md](./subagents.md) owns what a `rimz subagents` child adds on top. For users, the guide is [scripting.md](../../guide/scripting.md) and the flag reference is [cli/agents.md](../../reference/cli/agents.md#supervised-runs--p).

## What a supervised run is

`rimz agents <spec> <prompt> -p` gives a caller the contract of `claude -p`: one prompt in, one answer on stdout, one exit code to branch on. The turn runs in a real pane in the caller's room, as a fleet member with a card, a handle, and a transcript.

A pane gives no completion signal of its own. A headless process reports that it finished by exiting, but an agent CLI stays alive after its turn, the pane keeps a shell, and the process can outlive the work or die before reporting it. A run therefore needs an external completion signal, and that signal has to survive the caller crashing, the pane closing, and the machine rebooting.

The rule that resolves this is that the durable run record is the run, and the pane, the wrapper, and the wake socket are latency. A `RunRecord` is written before the pane opens, and every status change is a locked read-modify-write on that file. The agent's own lifecycle hooks fold the completion into it, which is why `-p` refuses an agent without [installed and trusted hooks](../agents/adapter.md#hook-install). Everything else on this page either produces that record or reacts to it.

`-p` adds no execution engine of its own. It sequences parts the harness already has: the launch path, the budget gates, the message path, and the exec wrapper.

## Module layout

| File | Owns |
| --- | --- |
| [`store/run.rs`](../../../crates/rimz/src/store/run.rs) | The durable record and its wire: `RunRecord`, `RunStatus` and its exit codes, `ReportTo`, `RunVerify`, the temp-file-plus-rename codec, the `run_completed` wake frame with its sender `wake_run`, and the waiter probe `run_waiter_is_live`. |
| [`harness/run.rs`](../../../crates/rimz/src/harness/run.rs) | Policy over that record: `SupervisedRunRequest`, the locked `create` and status transitions, the lifecycle fold, verify settlement (`settle_verify`), the stranded-park settle, cancellation, response-file publishing, the startup relaunch rule, and the lock-free `load` and `list` wrappers. |
| [`harness/run/report.rs`](../../../crates/rimz/src/harness/run/report.rs) | The `joined_at` and `report_message_id` stamps that joins and subagent digests share. |
| [`harness/run/peer.rs`](../../../crates/rimz/src/harness/run/peer.rs), [`harness/run/team.rs`](../../../crates/rimz/src/harness/run/team.rs) | Launcher-opened peer turns ([subagents.md](./subagents.md#the-lifecycle-end-to-end)) and the team-long leader run ([teams.md](./teams.md#team-reports)), both stored as `RunRecord`s. |
| [`harness/owed.rs`](../../../crates/rimz/src/harness/owed.rs) | `owed_wake`: whether a session is still owed a harness wake, which decides a [park](#parked-runs). |
| [`harness/fleet.rs`](../../../crates/rimz/src/harness/fleet.rs) | `FleetRuns`, the settlement rule that selects a launcher's fleet runs ([subagents.md](./subagents.md#the-lifecycle-end-to-end)). |
| [`harness/run_wake.rs`](../../../crates/rimz/src/harness/run_wake.rs) | The receiving half of the blocking wait: the per-run datagram socket, frame validation, the poll loop, and the timeout and cancellation transitions it drives. |
| [`harness/run_timeout.rs`](../../../crates/rimz/src/harness/run_timeout.rs) | Producer-side deadline detection and spawning the detached timeout helper. |
| [`harness/deadline.rs`](../../../crates/rimz/src/harness/deadline.rs) | Pure deadline policy: the warning and stop rungs, the stop channel, and the grace-bound kill time. |
| [`harness/park_notice.rs`](../../../crates/rimz/src/harness/park_notice.rs), [`harness/stall_notice.rs`](../../../crates/rimz/src/harness/stall_notice.rs) | Producer-side detection of a launched child parked on a provider limit or gone silent, the notice text, and spawning the detached notice helper ([backstops](./subagents.md#backstops)). |
| [`harness/plan.rs`](../../../crates/rimz/src/harness/plan.rs) | `plan_supervised_launch`: lane qualification, isolation inheritance, route, resolve, single-cell refusals and the child permission cap before finalize, then the isolation cap. Warnings and child-mode notes return as values. |
| [`harness/prompt_compose.rs`](../../../crates/rimz/src/harness/prompt_compose.rs) | `retry_prompt` and `verify_reprompt`, beside the system-prompt composition [fleet.md](./fleet.md#system-prompt-composition) owns. |
| [`cli/supervised.rs`](../../../crates/rimz/src/cli/supervised.rs) | Command-neutral effects: agent and program preflight, the pane's exec argv (`run_pane_cmd` over an `ExecRequest`), stop and cancel (`stop_supervised_run`, `StopRunErr`), the SIGINT cancellation flag, `--timeout` parsing, and the stream-json prompt reader. |
| [`cli/supervised/run.rs`](../../../crates/rimz/src/cli/supervised/run.rs) | The driver `run_supervised` that `agents -p`, loop fires, and workflow steps call: preparation around the launch plan, room birth, placement, the attempt loop, the verify loop, and the retry loop. |
| [`cli/supervised/output.rs`](../../../crates/rimz/src/cli/supervised/output.rs) | The output projections: text, JSON, the `StreamSink` for NDJSON `RunStreamEvent`s and streamed text, and the stderr forensics block. |
| [`cli/supervised/stream.rs`](../../../crates/rimz/src/cli/supervised/stream.rs) | Streaming while a run is live, for a blocking caller and for an attached `agents wait --stream`. |
| [`cli/supervised/verify.rs`](../../../crates/rimz/src/cli/supervised/verify.rs) | Running the verify command and delivering its re-prompt. |
| [`cli/supervised/pane.rs`](../../../crates/rimz/src/cli/supervised/pane.rs) | Subagent-zone placement, finding the run's pane, capturing its failure tail, closing it, and the stop backstop. |
| [`cli/agents_cmd/exec.rs`](../../../crates/rimz/src/cli/agents_cmd/exec.rs) | The in-pane wrapper's store side: recording its pane and provider process, the process-death backstop, and settlement. |
| [`cli/agents_cmd/exec/supervisor.rs`](../../../crates/rimz/src/cli/agents_cmd/exec/supervisor.rs), [`exec/duty.rs`](../../../crates/rimz/src/cli/agents_cmd/exec/duty.rs) | The parked wrapper image: `supervise_child` and its `RunMonitor`, and the hidden `agents supervise-duty` helpers it delegates store work to, including the stranded-park check. |
| [`cli/agents_cmd/run_timeout.rs`](../../../crates/rimz/src/cli/agents_cmd/run_timeout.rs) | The hidden `agents run-timeout` helper that settles an overdue run. |
| [`cli/agents_cmd/park_notice.rs`](../../../crates/rimz/src/cli/agents_cmd/park_notice.rs), [`stall_notice.rs`](../../../crates/rimz/src/cli/agents_cmd/stall_notice.rs) | The hidden `agents park-notice` and `agents stall-notice` helpers that claim a child's notice on its run record and tell its parent. |
| [`cli/agents_cmd/wait.rs`](../../../crates/rimz/src/cli/agents_cmd/wait.rs), [`stop.rs`](../../../crates/rimz/src/cli/agents_cmd/stop.rs) | Joining on runs started elsewhere, and cancelling them. |

## The record

`RunRecord` carries the run's identity, its launch choices, and everything an inspection surface needs after the pane is gone. The same schema carries launcher-opened peer turns (`peer`) and the team-long leader run (`team`). A record with neither is a plain supervised run, and the rest of this page describes that shape.

| Group | Fields |
| --- | --- |
| Identity | `run_id`, `workspace_id`, `kind`, `agent_id`, `agent_name`, `pane_id` |
| Provider process | `provider_pid`, `provider_process_start` |
| Provenance | `prompt`, `worktree_path`, `permission_mode`, `budget`, `retry_of`, `loop_task`, `workflow` |
| Retention | `keep`, `subagent` |
| Outcome | `status`, `last_message`, `parked_answer`, `verify`, `failure_tail`, `transcript_path` |
| Accounting | `cost_usd`, `input_tokens`, `output_tokens` |
| Reporting | `reader`, `report_to`, `joined_at`, `report_message_id` |
| Follow-ups | `follow_ups`, `follow_up`, `opened_by`, `earlier_answers` ([subagents.md](./subagents.md#follow-ups-and-resume)) |
| Other run shapes | `peer` ([subagents.md](./subagents.md#the-lifecycle-end-to-end)), `team` ([teams.md](./teams.md#team-reports)) |
| Timing | `started_at`, `deadline_at`, `updated_at`, `completed_at`, `parked_at` |
| Subagent ladder and notices | `timeout`, `warn`, `grace`, `deadline_notice_at`, `park_noticed_activity`, `stall_noticed_at` ([subagents.md](./subagents.md#backstops)) |

`agent_id` starts empty. The first lifecycle observation that matches the run fills it, which is how the record binds to a session whose id did not exist when the record was written. `transcript_path` points at the provider's own session file, which streaming reads directly. The RimZ transcript log that `rimz transcript` renders is a different file. The wrapper stamps `provider_pid` and its process-start token when it spawns the provider, so a later signal cannot reach a reused PID.

`report_to` says who is owed the answer. It is `launcher` when absent, and `nobody` for an `agents` or `teams` launch with `--detach` and for a workflow step. A `nobody` run is never listed in a digest and never holds its launcher's turn open. `reader` is the launcher's handle, which names the `out/<reader>/` directory a response file lands in.

Records live at `~/.rimz/ws/<workspace-dir>/owned/runs/<run_id>.json`. GC removes a terminal record once its file is older than 7 days ([store.md § What is on disk](../store.md#what-is-on-disk)), so a record outlives its pane and caller by a week, not forever. `store::run` owns the schema and the codec, which publishes through the store's fsyncing temp-file-plus-rename helper. `harness::run` owns every transition and takes the workspace lock for it. Its `create`, `load`, and `list` wrappers let a caller holding `StatePaths` reach the codec without resolving the runs directory. Store reset is the one writer outside `harness::run`: under the same lock, it cancels every active run before rotating room state.

A write that newly makes a run terminal does two more things in `harness::run::write_record` and `update_record`. It publishes the answer to a response file under `out/<reader>/` for a subagent, a peer turn, a team run, or a `nobody` run, before the terminal record lands, so a reader that sees the terminal record finds the file ([subagents.md](./subagents.md#the-lifecycle-end-to-end) owns the file naming). It also wakes the workflow attempt named in `workflow`, if any ([workflow.md § Wakes](./workflow.md#wakes)). When the lifecycle fold ends a run that is not a peer turn, `run::settle_lifecycle` then records spend: `cost_usd` from the session's context record, and `input_tokens` and `output_tokens` from the transcript's session totals. Peer turns skip it because those figures are session-cumulative.

Live agent state stays out of the record. `rimz agents show <run-id>` reads the record and attaches the live card from the snapshot at read time (`harness::run::live_status`), so agent activity costs no locked writes.

## Status and exit codes

`RunStatus` is the state machine, and `RunStatus::exit_code` is the whole caller-facing contract.

| Status | Exit | Reached by |
| --- | --- | --- |
| `Pending` | (124) | the record is written and the agent has not reported yet |
| `Running` | (124) | the first non-terminal lifecycle observation, a verify re-prompt reopening the run, or a subagent follow-up reopening it |
| `Completed` | `0` | a root `TurnEnded` that did not error, with no harness wake owed |
| `Failed` | `1` | a root `TurnEnded` that errored, a session `Ended` before any turn result, the wrapper's process-death backstop or launch error, a pane that failed to open, a verify re-prompt that could not be delivered, or a stranded park |
| `VerifyFailed` | `123` | the verify command stayed red through `--max-attempts` total turns |
| `TimedOut` | `124` | the blocking waiter's `--timeout` elapsed, or the timeout helper found `deadline_at + grace` overdue (absent grace is zero) |
| `BudgetExceeded` | `125` | the budget-park helper stopped the run's agent mid-turn for crossing a dollar cap: its launch `--budget`, the turn cap, or the room or account cap ([budget.md § The park](./budget.md#the-park)) |
| `Canceled` | `130` | `rimz agents stop`, Ctrl+C on a blocking caller, SIGTERM to a loop runner with a live supervised run, a `TurnInterrupted` signal, a subagent's parent ending, or store reset |

The parenthesized codes are what a caller exits with when it stops waiting on a live run.

Exit `125` also comes from a run that never started. The provider quota gate and `budget::scope_gate` refuse before any record exists and return `SupervisedRunOutcome::BudgetExceeded`, and `run_print` exits `125` straight from that outcome. A script reading run records never sees a gate refusal.

A terminal status is written once. `RunRecord::mark_terminal` returns false when the record is already terminal, and the transitions pass that on as a `wrote` flag. Two paths leave a terminal status:

- Verify settlement, only from exactly `Completed`: `run::settle_verify` moves the run back to `Running` for a re-prompt, or on to `VerifyFailed` at the attempt cap.
- A subagent follow-up: a matching `TurnStarted` on a terminal `subagent` run, whatever its terminal status, reopens it as `Running` in `fold_lifecycle` ([subagents.md § Follow-ups and resume](./subagents.md#follow-ups-and-resume)).

## Life of a run

The driver is `run_supervised` in `cli/supervised/run.rs`. One call can hold several attempts, and the order inside an attempt matters.

1. Prepare. Resolve the workspace and the caller, check a subagent against its parent's allowlist, then plan the one-cell layout with `plan::plan_supervised_launch` ([fleet.md § From spec to panes](./fleet.md#from-spec-to-panes)). The plan rejects a prompt that parses as a spec token (`plan::reject_prompt_that_looks_like_spec`), which catches a spec typed where the prompt belongs, and returns its finalize warnings for the driver to print. Then preflight runs, before the multiplexer is touched: scoped capabilities, the launched account's home and hooks (`ProviderLogin::preflight`, for that one provider only), the adapter's hooks and turn-lifecycle signal (`preflight_hooks` with `TurnLifecycleNeed::Wired`), the launch directory and the provider's folder trust for it (`preflight_launch_dir` in `LaunchDirMode::Supervised`), the adapter's launch blockers, and the provider program resolving on `PATH` after shell startup at or above its version floor. An adapter with no verified turn-completion signal refuses here.
2. Birth the room. `RoomContext` preserves the room's account pins without freezing inherited defaults, and the provider quota gate runs once on the login resolved at preflight. A caller outside a room gets one. An attended caller (stdin is a terminal) may reset a stuck room, while a non-interactive caller gets an error asking for an explicit reset.
3. Gate on dollars. At the top of every attempt, the provider quota gate and [`budget::scope_gate`](./budget.md#the-fail-fast-gate) run again. A refusal returns `SupervisedRunOutcome::BudgetExceeded` without writing a record or opening a pane.
4. Build the identity. A `RunRecord` is built in memory, with `deadline_at` set to `started_at` plus `--timeout` when given. The store opens a launch batch that mints the provisional agent row and name, and that name is stamped onto the record. On a retry attempt an explicit `--name` becomes a soft name, so the fresh attempt can mint again while the prior ended row keeps its handle.
5. Bind the waiter. A blocking run installs its SIGINT flag and binds its wake socket before the record exists and before the pane opens, so no completion can land in a gap. A `--bg` run binds nothing.
6. Write the record, then open the pane. `run::create` persists the record, then the pane opens running the exec wrapper with the run id in its request. Placement follows `run_placement`: `--new-tab`, a request marked `force_new_tab` (loop-owned and workflow runs), or a caller with no pane gets a new unfocused tab. Otherwise a `rimz subagents` child goes to the [subagent zone](./subagents.md#pane-zones), and everything else splits the current tab without taking focus. A new tab is titled `loop <task>` for a loop-owned run, the companion base for a child (`#<channel> subagents`, or the main-channel view-derived name), and `run <kind>` otherwise. A pane that fails to open marks the run `Failed` with the error as its reason and fails the launch batch.
7. Return or wait. A `--bg` run returns here with the agent name, the run id, and, for an agent-launched run, its response path. A blocking run waits until the record is terminal, then captures a pane tail for a non-completed record that has none (`record_failure_tail_before_cleanup`).
8. Verify. With `--verify` and a `Completed` record, the [verify loop](#verification-re-arms-the-same-run) runs.
9. Reclaim. The driver stamps the result joined, then closes the run pane unless `--keep` ([Reclaiming the run pane](#reclaiming-the-run-pane)).
10. Retry or finish. A `Failed` run with retries left prints its forensics and starts a new attempt at step 3. Anything else returns the record, which the caller projects to stdout and turns into an exit code.

## Completion: folding lifecycle observations

`run::record_lifecycle` folds one agent lifecycle observation into the record. It returns `Some(record)` only when that observation newly made the run terminal, which is what lets the writer send exactly one datagram. The hook path calls it through `run::settle_lifecycle`, which adds the spend and the workflow wake.

The fold filters before it classifies:

- An observation carrying a `parent_agent_id`, or a `SubagentStarted` or `SubagentStopped` signal, is dropped. A child's completion never finishes its parent.
- An observation of a different `kind` is ignored, and so is one arriving after the record is terminal, except the `TurnStarted` that reopens a subagent.
- A team run takes `fold_team_lifecycle` instead, which keeps the leader's newest final message and never settles the run ([teams.md](./teams.md#team-reports)).
- Session binding is strict once made. An unbound record adopts the observed `agent_id` and name. A bound record ignores an observation for a different session id and an observation with no session id, so a same-kind descendant with its own session cannot end the run.

What passes is classified by `LifecycleSignal::terminal_disposition` in `agents::lifecycle`, and `fold_lifecycle` maps the disposition to a status:

| Lifecycle signal | Result |
| --- | --- |
| `TurnEnded { errored: false, parked_on_background: false }`, nothing owed | `Completed` |
| Same clean end, harness wake owed | stays `Running` with `parked_at` set: the transcript path and the answer so far are stored, and no wake is sent |
| `TurnEnded { errored: true, parked_on_background: false }` | `Failed` |
| `TurnEnded { parked_on_background: true }` | not terminal: the agent parked on background work and ends the turn later |
| `TurnInterrupted` | `Canceled` |
| `Ended` | `Failed`: the session ended without reporting a turn result |
| `TurnStarted`, `ToolUsed`, or `AwaitingInput` on a parked run | clears `parked_at`, seals a non-empty `last_message` into `parked_answer`, stays `Running` |
| anything else | not terminal: promotes `Pending` to `Running` and records the first transcript path |

A terminal fold also stores the transcript path and the final assistant message. `record_assistant_message` lets an adapter store its declared final output earlier without ending the run. Both write through `set_turn_message`, so `last_message` is the whole answer: `parked_answer`, then `"\n\n---\n\n"`, then this turn's message. A later message of the same turn replaces only this turn's part, an empty one adds nothing, and a terminal fold with no message keeps what is stored. A single-turn answer is the turn's message alone.

Process death is a backstop and never reads as success. When the provider exits, a fresh launch whose run is still `Pending` may first be relaunched in the same pane ([fleet.md § A startup death is relaunched first](./fleet.md#a-startup-death-is-relaunched-first)). Otherwise the wrapper drains queued hooks (`hook_drain::drain_through`), then waits what is left of `RUN_EXIT_TERMINAL_GRACE` (500 ms) for a terminal record (`fail_run_if_child_exited_first` in `cli/agents_cmd/exec.rs`). If none lands, it captures its own pane tail, writes `Failed` through `fail_if_nonterminal`, and wakes the waiter. A provider that exits cleanly without a terminal hook is still a failed run.

A wrapper that fails before or while supervising the provider settles in one place. Every error `run_exec` returns once the run context exists marks a still-provisional launch failed and calls `fail_if_nonterminal` with the error as the reason: one locked write sets `failure_tail` and `Failed` together, so the waiter that the terminal record releases reads the cause with it. The reason is the error chain the wrapper prints on stderr, outermost first and joined by `: ` (`render::error_line`), for example `reading launch prompt <path>: No such file or directory (os error 2)`. An existing tail wins, and a run that is already terminal keeps its status and gains no reason. The launcher's own failure to open the run pane fails the run through the same transition. A captured pane tail is the pane's last 60 lines of scrollback and screen, after trailing blank rows are dropped (`cli/supervised/pane.rs::capture_failure_tail`), so output scrolled off a short pane still reaches the tail. Any failure tail is capped at its last 4 KiB (`FAILURE_TAIL_CAP`).

## Parked runs

A clean turn end does not complete a run while its session is still owed a harness wake. `harness::owed::owed_wake` decides that, and the hook calls it only for a root `Completed` disposition, before the fold takes the run's workspace lock. A read error logs and counts as nothing owed. The signal classifier stays pure, and failures and cancellation are never held open.

| `OwedWake` | Owed when |
| --- | --- |
| `Subagents` | a launched fleet member has a run that is live or still owes a report, as `FleetRuns` selects them ([subagents.md](./subagents.md#the-lifecycle-end-to-end)) |
| `Team` | a team this session launched has an open team run whose board is not `Done` (`fleet::owed_team_runs`) |
| `Wait` | the card has an armed one-shot wait |
| `WakeInFlight` | a harness `Wait`, `Signal`, `SubagentReport`, `AgentReport`, or `TeamReport` message for the card is in flight |

A run whose `report_to` is `nobody` counts toward neither the fleet nor the team term, so a launcher with only detached work running ends its turn and completes instead of parking. The fleet and team terms are independent, so a launcher that launched only a team is owed too.

The read order follows each producer's write order, so that a wake in flight always shows in at least one read:

1. The fleet term is read first, through `FleetRuns` over the Audit projection. The digest reporter stamps the child rows before it queues the digest, so rows found unstamped are owed, and rows found stamped are followed by a queue read that the digest has by then reached.
2. The team term follows. Its reporter queues before stamping the run, so an open run is owed unless its board already reads Done, and a settled run has its report in the queue read afterwards. This is why the queue read counts `TeamReport` without making it a per-member fleet digest.
3. The wait terms reuse `TurnWaitView::load`, whose catalog, queue, rollup order matches a wake's own publish order.

A park keeps the record `Running`: `parked_at` is an optional, defaulted timestamp, not a new status. The first in-turn signal (`TurnStarted`, `ToolUsed`, or `AwaitingInput`) clears it, and every terminal write clears it too. A keepalive turn's start is a ping edge the hook never folds into the run, so its first tool hook clears the park instead. Registration and compaction signals can arrive at rest and leave the park unchanged. `agents show` displays the park's age, and record JSON and NDJSON live status carry `parked_at`. Live status keeps reporting the card's raw rollup status, unchanged by this fold, so the timestamp is the park evidence.

A park does not end the answer. The text the parked turn produced is the start of it, and the resumed turn's message joins after it. The in-turn signal that clears the park seals the answer so far into `parked_answer`, which the terminal write keeps and only a reopen clears. Sealing when work resumes, rather than at the park, lets a replayed `Stop` of the parked turn replace its own text instead of adding a copy. A later in-turn signal finds `parked_at` already clear and seals nothing.

A park can strand: the wake it waits for never comes. While a wrapped run is parked and non-terminal, its wrapper keeps one resident `supervise-duty Strand` helper (`cli/agents_cmd/exec/duty.rs`), which checks the park every `PARK_STRAND_POLL` (5 s) while the wrapper is its parent. A helper that fails is respawned with a backoff that doubles up to 60 s. Two consecutive nothing-owed checks under the same `parked_at` call `run::settle_stranded_park`. Under the workspace lock it compares `parked_at` and non-terminal status, writes `Failed` and the failure tail together, then wakes the waiter after unlocking. The reason is `parked on a wake that never arrived: no armed wait, no wake in flight, no live subagents`, and an existing failure tail wins. An owed wake, a missing bound identity, or a read error does not settle the park.

Before each strand check the helper repairs its own fleet, because a fleet that settled while its digest was lost is owed and unresolvable: the parked parent is at rest waiting for exactly that digest. The repair settles the session's ended team cohorts, then runs the fleet digest reporter for it. A team whose board is Done settles completed. Otherwise it fails with `cohort ended before Done`, queuing one failure `TEAM_REPORT` with the leader's last response. This is the same idempotent repair the sidebar producer's `orphan_sweep` backstop runs, and its terminal CAS tolerates a concurrent reporter. Success turns the owed `Subagents` or `Team` term into `WakeInFlight`, so the run completes on the report's turn instead of failing. Every outcome and error is logged and ignored: the repair never gates the check that follows it.

A park has no age bound: an armed `rimz wait --in 8h` is a legitimate park of any age. Its exits are the wake itself, the strand settle, the digest repair, `--timeout` where one was given, and `rimz agents stop`.

The compare-and-swap relies on the hook folding `TurnStarted` before it marks the wake message `Delivered`, so seeing the wake gone means the park clear already happened. A keepalive-opened turn has no wake message. There the order is its first tool hook clearing `parked_at` before that tool can cancel the owed wait, so seeing the wait gone likewise means the clear already happened. The second look covers the digest reporter's short stamp-then-queue gap across two lock holds. A clean parent end inside that gap can still complete before the digest is queued, and the fold accepts that gap rather than reorder the reporter's durability sequence.

## The wake socket

`run_wake::RunWaiter` exists only to cut the latency of a polling loop that is already correct.

The waiter binds `sock/run.<short_id>.sock` in the runtime directory, removing a stale file left by a crashed waiter, and keeps it bound across verify re-prompts. A writer that newly makes a run terminal from another process calls `store::run::wake_run`, which sends a `run_completed` datagram to that path if the socket file exists. Writes the waiting process makes itself send none: its `--timeout` transition (`run::timeout`) and its verify settlements. The waiter checks every frame against `(workspace_id, run_id)`, logs and drops a mismatched or unparseable frame, and keeps receiving.

The record on disk stays the truth. `wait_terminal` reloads it every `RUN_WAIT_POLL` (250 ms) whether or not a datagram arrived, so a lost, late, or mismatched datagram costs at most one tick. Streamed and plain callers use the same `wait_terminal` call, and a streaming caller passes an observer closure that sees each reloaded record.

The waiter also writes two transitions itself. When the SIGINT flag from `install_run_interrupt_flag` is set, it calls `run::cancel_and_wake`, which wakes only on a newly written cancellation. When the caller's `--timeout` elapses, it writes `TimedOut`.

A loop-owned run also takes SIGTERM into that flag, from the moment its waiter binds through verification, settle and re-prompt waits, inline join, and pane cleanup. The stop ladder's SIGTERM then cancels a live run whose record appeared after the ladder's own cancel looked for it, so the runner does not end and leave the agent running unsupervised. After settlement, the signal leaves the run's result unchanged. The handler stays installed through the loop's terminal record and run-lock release, so a late SIGTERM cannot replace a settled result with a stop-ladder row.

A second path enforces deadlines without any waiter. On its heavy-lane refresh, the room host calls `run_timeout::enforce`, which lists run records and spawns the hidden `agents run-timeout` helper when `deadline::kill_due` or `deadline::stop_due_by_pane` is true. This covers background runs, which have no waiter, and blocking runs whose caller died. Subagents have a [warning and stop ladder](./subagents.md#the-lifecycle-end-to-end) before the kill. Other supervised runs have no grace. At the kill bound the helper rechecks under the workspace lock (`timeout_if_due`) and writes `TimedOut`, storing the last non-empty assistant message from the provider's transcript when the record has none. It then wakes any waiter, sends `SIGTERM` to a subagent's recorded provider process when the PID and start token still match, and reclaims the pane unless the run is a kept subagent. Detection stays read-only in the sidebar process, and the short-lived helper owns every mutation.

`deadline_at` is fixed when the attempt starts, and a verify re-prompt does not move it, so the helper can time out a run in the middle of its verify rounds even though each blocking wait gets the full `--timeout`.

## Verification re-arms the same run

`--verify <CMD>` runs a shell command in the run's working directory after a completed turn. A red result reuses the same agent session instead of starting a new one.

```text
turn completes
  └─ run verify command in the run cwd
       └─ run::settle_verify stores the evidence and settles the record
            ├─ canceled ───────► record becomes `canceled`
            ├─ passes ─────────► record stays `completed`
            ├─ attempt == max ─► record becomes `verify_failed` (exit 123)
            └─ red, attempts left: `completed` → `running` (VerifyStep::Reprompt)
                 ├─ deliver the verify_reprompt through message::synthetic::deliver_now
                 │    into the same pane and the same agent session
                 └─ wait on the same bound socket for the next root TurnEnded
```

No provider resume and no replacement pane enter this path, so the next `TurnEnded` makes the same record terminal again and wakes the same socket. `--max-attempts` counts total agent turns, defaults to `3` (`VERIFY_MAX_ATTEMPTS_DEFAULT`), and must be at least `1`. The verify command runs under `--timeout` when set and `CHECK_DEFAULT_TIMEOUT` (5 minutes) otherwise, and a timed-out verify is red. The re-prompt carries the command, its exit status, and a 4 KiB output tail, and it goes through the [message path](./messaging.md) with gate `Any`. A re-prompt that is queued but not delivered fails the run with the delivery error as its reason.

A cancellation observed during verification stores the evidence, then cancels. A SIGTERM to the waiting `rimz agents run` process while its verify command runs counts as that cancellation: the run ends `canceled` and is never re-prompted. A run in verify already reads `completed`, so this SIGTERM is the only stop that reaches it, and `rimz loop stop` and a loop's replace rely on it.

`--retries <N>` is a separate loop. It reruns only a `Failed` run (`RunStatus::is_retryable`) in a fresh session and pane. The new prompt is the original one plus a `<previous-attempt-failure>` block holding the previous attempt's `failure_tail`: the launch error when the wrapper failed the run, the captured pane tail otherwise. Each attempt writes its own record with `retry_of` pointing at its predecessor, and the last attempt decides the exit code. Timeouts, budget stops, and cancellations are never retried. The two loops compose: a verify repair stays in-session after a completed turn, while a retry starts a fresh session with the verify attempt count reset.

Both loops need a blocking run, so both refuse `--bg` and `--output-format stream-json`. The `--bg` conflict is declared on the clap arguments (`conflicts_with = "bg"`), and the stream-json refusals and the `--max-attempts` checks live in `validate_supervised_output`. The user gets the error at the entry point, because the background path returns before the verify phase and before any retry could fire.

## Output and input projections

`--output-format` chooses what a blocking caller prints. All three formats read the same record.

| Format | Prints |
| --- | --- |
| `text` (default) | the run's answer (`last_message`, every turn of a [parked run](#parked-runs) in order) on stdout, and nothing else |
| `json` | the full run record, pretty-printed |
| `stream-json` | NDJSON `RunStreamEvent`s as the turn runs: `message` per new assistant message, `status` when the live card changes, and one `end` with the status and last message |

Text output keeps stdout as the answer channel. A run that did not complete prints a stderr forensics block (`print_run_forensics`): the status and exit code, the failure reason (the wrapper's launch error or the captured pane tail), the failed verify result, and the transcript path. A completed run with no extracted message prints a one-line stderr note instead.

`--input-format` chooses the prompt source. `text` takes the positional prompt, followed by `--stdin` content inside `<stdin>` tags when both are given. `stream-json` reads `{"type":"user"}` messages from stdin until EOF and joins their text (`read_stream_json_prompt`). It refuses a positional prompt and `--stdin`.

Streaming reads the provider's transcript at `record.transcript_path`. Both `--output-format stream-json` and `agents wait --stream` hold an `agents::transcript::TranscriptCursor`, which asks the adapter's `read_assistant_transcript_page` for assistant messages appended since its position. The cursor is keyed by path and session id, restarts when either changes, and rewinds when the source shrinks below it. A blocking caller streams from the wake loop's observer, while `agents wait --stream` polls every 500 ms instead (`stream_attached_run`). `agents wait --stream --json` emits the same NDJSON events, and plain `--stream` renders assistant text. The wake socket plays no part in streaming.

## Launch options on `-p`

A run launches through the same layout resolution and finalization as an interactive launch, so model, effort, profile `auto-compact`, system-prompt files, and the adapter's refusal of a setting it cannot render all behave as [fleet.md § From spec to panes](./fleet.md#from-spec-to-panes) describes. Four things are specific to `-p`:

- An unset permission mode defaults to `auto`, and a profile or virtual cell keeps its declared mode. Explicit `--ask` or `--yolo` replaces either declaration, and the two flags conflict. For a subagent with a caller, the planner caps this choice against the caller's recorded mode before finalization (unrecorded is auto), refusing an explicit higher mode and lowering a higher default. Peers, loops, workflows, and caller-less launches are not capped. `rimz subagents` exposes only `--yolo`, not `--ask`.
- `--max-turns` renders through the adapter's turn-limit argv, and an adapter without one refuses it at finalization.
- The layout must resolve to exactly one agent cell and no other cells.
- `--fresh` accepts a named worktree (`-w NAME`). `--json` is refused, and JSON output is `--output-format json`.

Named-worktree entry uses the [shared launch check](../../reference/cli/agents.md#relaunch-into-a-named-worktree) before checkout resolution. An unattended existing-tree refusal exits `2` without a run record. Its fresh command keeps `-p`, the positional prompt, and the stdin mode, but never includes stdin content. Harness-owned loop and workflow runs skip this check.

The per-kind argv mappings live in the adapter pages indexed under [the agent layer](../README.md#the-agent-layer).

## Background runs and joining

`--bg` splits starting a run from waiting on it. The driver opens the pane, prints the agent name, and returns without binding a waiter. `rimz agents wait <ref>...` then polls the durable records every 500 ms, settling when all targets finish or, with `--any`, when the first one does. A reference is the printed name, a run id, or any [agent address](./fleet.md#the-address), so one handle works for `wait`, `show`, `stop`, and `message`.

`resolve_wait_target` decides whether a reference waits on a run or on an agent. It waits on the newest run the reference names when that run is live, when no live agent matches, when the reference is the run id itself, or when the run is a subagent's and the live agent is that launched child. Otherwise a reference to a live agent whose newest run is already terminal waits on the agent, and settles when the agent reaches its done gate or fails.

Because the record is the run, a `wait` in another shell or a later CI step sees the same outcome the launching process would have seen. The wake socket is per host, and only the polled record is shared.

`wait` never owns lifecycle transitions. It maps a settled status to an exit code, and with several targets exits with the first non-completed target's code. When it prints a result to an attended caller it stamps `joined_at`, and it cancels a queued subagent digest whose every row has been joined ([subagents.md § Joins, stops, and the digest](./subagents.md#joins-stops-and-the-digest)). It never signals a process, cancels a run, or closes a pane. Three consequences follow from that:

- Background [pane reclamation](#reclaiming-the-run-pane) belongs to the in-pane wrapper, not `wait`. A subagent without an attended join waits for digest receipt instead.
- `--any` returns the first finisher and leaves the others running to their own completion, which is why the guide's [race recipe](../../guide/scripting.md#in-a-pipeline) stops the loser explicitly.
- `wait --timeout` bounds the caller's patience, not the run. It exits `124` with the run still live, the same code a `TimedOut` run exits with. Only the full run record tells the two apart (a run the caller stopped waiting on is still `running`), so a script that needs the difference rereads it under `run` in `rimz agents show <ref> --json`.

`wait --json` prints a compact map keyed by the typed reference, whether the wait has one target or several. Each entry (`WaitEntryJson`) carries the pet `name` (the session id for an unnamed agent, the run id for an unnamed run), the `status` and its `exit` code, and, when known, `cost`, `transcript_path`, `last_message`, `error`, and a blocked target's `reason`. A target still unfinished when `--timeout` elapsed reads `timed_out`. Identical typed references are deduplicated before target resolution.

`TurnWaitView` projects why an agent's turn stays open, including a wake in flight that the card alone cannot show. The CLI retains the last typed reason per target, prints a stderr diagnostic only when it changes, and flushes before exiting.

## Reclaiming the run pane

Cleanup is best-effort and split by who is still alive to do it.

A blocking run's driver reclaims the pane after the verify phase. First, `join_presented_attempt` stamps `joined_at` on a terminal record, since the driver prints that result itself, so a launching agent's fleet digest does not list it again. Then, unless `--keep`, `close_run_pane` closes the recorded `pane_id`, and falls back to the agent row found by `(kind, agent_id)` in the snapshot when no pane was recorded or closing it failed. A hibernated pane is woken before it is closed, and a run that predates the room's last rebirth is left alone. A `Canceled` record goes through the stop backstop below instead. The failure tail the driver needs was captured when the wait returned (step 7 of [Life of a run](#life-of-a-run)). `record_failure_tail` keeps the first tail written, so the wrapper's launch error or the tail it captured as it died is never overwritten. A run that ends `VerifyFailed` after a completed turn gets no pane tail: its evidence is the stored verify result.

The in-pane wrapper reclaims its own pane when the request set `exit_on_run_completion`: every `--bg` run, every `rimz subagents` child, every loop-owned run, and every workflow step, unless `--keep`. `supervise_child` in `cli/agents_cmd/exec/supervisor.rs` rereads the record every `RUN_MONITOR_POLL` (250 ms) with self-cleanup and every 5 s without it. Once the record is terminal and no waiter socket is live (`run_waiter_is_live`), a non-subagent wrapper sends the provider `SIGTERM`, escalates to `SIGKILL` after `CHILD_SIGNAL_GRACE` (300 ms), runs worktree cleanup for a marked worktree, and closes its own pane. A subagent wrapper takes that path only after the parent has received the answer and its receiving turn has ended ([subagents.md § The lifecycle, end to end](./subagents.md#the-lifecycle-end-to-end) owns the receipt checks). The live-waiter check leaves a blocking caller in charge of verification and cleanup, and the wrapper takes over only when that caller is gone.

A stop, or a Ctrl+C that cancels a blocking caller, writes `Canceled` through `cancel_and_wake` and then runs `close_stopped_run_pane_after_grace`, whose result is the stop's result. It polls the authoritative pane listing (`PaneReadConsistency::RequireAuthoritative`, never the pushed topology cache) every 250 ms for `STOP_BACKSTOP_GRACE` (3 seconds). A listing without the pane means closed, and so does a listing that fails with `MuxErr::SessionNotFound`, since a session that is gone holds no pane. Any other listing error is retried, never returned. At the deadline, a pane still listed, or one no listing could be read for, is closed. A close that fails is followed by one more listing, and the stop fails with `StopRunErr::PaneOpen` unless that listing lacks the pane or reports the session gone. The same error covers a multiplexer that could not be resolved. The blocking driver's own cleanup logs these errors and continues.

Two limits follow from that rule:

- Success means no pane RimZ can name remains, which is weaker than no pane. A run with no recorded pane and no agent-row pane at the deadline succeeds, so a pane whose wrapper never recorded it (`record_own_run_pane` is best-effort) and whose agent never registered outlives a successful stop.
- Only Zellij reports a missing session as `MuxErr::SessionNotFound`. tmux answers a listing for one with a plain command error (`can't find window: <session>`, or `error connecting to <socket>` with no server), so a kept run whose tmux session is gone fails each stop with `StopRunErr::PaneOpen`.

A stop on a terminal `--keep` record leaves the status alone and reclaims the pane, whether the reference was the run id or the agent name. The timeout helper ends with the same backstop, so a wedged wrapper cannot leave an overdue pane behind.

After the backstop succeeds for a subagent, `cli::supervised::stop_supervised_run` ends the child's still-open audit row with `rimz.subagent-stopped`, so a dead or lingering wrapper cannot leave a live card behind a gone pane. The row is the one with the run's session id, else the newest same-kind row with the run's name that registered at or before the run finished (`RunRecord::matches_stopped_agent`). That bound applies to a run already finished when the stop began, and a run still live then keeps the unbounded name match. A stop that fails with `StopRunErr::PaneOpen` records no end, and a failure to record the end fails the stop with `StopRunErr::NotEnded`. Root runs get no such row.

A `rimz subagents` child differs from a plain run in its retention. It reports before provider exit and can take follow-ups, and a parent's message can resume an ended child. `--keep` disables automatic completion cleanup and holds the pane past provider exit until its parent ends or Ctrl-C is pressed in the pane, without declining the answer. Every child has a parent watchdog, and the producer's orphan scan backstops missed digests and orphans, kept children included. [subagents.md § The lifecycle, end to end](./subagents.md#the-lifecycle-end-to-end) owns these rules.

Worktree cleanup depends on retries. A run without `--retries` marks its pane for wrapper-side worktree cleanup. A run with `--retries` leaves the mark off and has the driver clean up after the final attempt, so an intermediate failure does not remove the tree the next attempt needs.

## Runs the scheduler starts

A loop fire calls the same `run_supervised` driver with a request shaped by `schedule::runner::shape_loop_owned`. `loop_task` names the task so its records can be found, a scheduled fire without a task timeout takes the configured default, and wrapper self-cleanup is on unless the task keeps its pane. The shaper also sets `force_new_tab` for scheduled mode. A prompted `rimz agents` launch made by a loop check converts into that scheduled shape even under a manual fire. Any loop-owned request that opens a tab names it `loop <task>`. A budget refusal becomes one history row instead of an exit code. The scheduler's [placement contract](./loops.md#where-a-scheduled-run-lands) owns focus and sidebar behaviour.

A workflow step uses the same driver as a background run with self-cleanup, `force_new_tab`, `report_to = nobody`, and its `WorkflowAttemptRef` in `workflow` (`cli/workflow/host.rs`). Its terminal write wakes the workflow engine, which maps the status to the attempt's end ([workflow.md § Agent attempts](./workflow.md#agent-attempts)).

## See also

- [fleet.md](./fleet.md): the launch, address, exec wrapper, and cleanup machinery a run rides on.
- [subagents.md](./subagents.md): what a `rimz subagents` child adds to a run.
- [loops.md](./loops.md): driving these runs on a clock.
- [workflow.md](./workflow.md): runs as workflow step attempts.
- [messaging.md](./messaging.md): the delivery path a verify re-prompt uses.
- [budget.md](./budget.md): the gates and the park behind exit `125`.
- [store.md](../store.md): where run records sit among the other durable state, and when GC removes them.
- [cli/agents.md](../../reference/cli/agents.md#supervised-runs--p): every flag on `-p`, `wait`, `show`, and `stop`.
