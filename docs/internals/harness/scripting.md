# Supervised runs

> One supervised turn end to end: the durable record, the completion signal, the wait, verification and retry, the output projections, and pane reclamation. [fleet.md](./fleet.md) is the map for this area and owns the launch machinery a run rides on; [subagents.md](./subagents.md) owns what a `rimz subagents` child adds on top. For users, the guide is [scripting.md](../../guide/scripting.md) and the flag reference is [cli/agents.md](../../reference/cli/agents.md#supervised-runs--p).

## What a supervised run is

`rimz agents <spec> <prompt> -p` gives a caller the contract of `claude -p`: one prompt in, one answer on stdout, one exit code to branch on. The turn runs in a real pane in the caller's room, as a fleet member with a card, a handle, and a transcript.

A pane gives no completion signal of its own. A headless process reports that it finished by exiting, but an agent CLI stays alive after its turn, the pane keeps a shell, and the process can outlive the work or die before reporting it. A run therefore needs an external completion signal, and it has to survive the caller crashing, the pane closing, and the machine rebooting.

The rule that resolves this: **the durable run record is the run; the pane, the wrapper, and the wake socket are latency.** A `RunRecord` is written before the pane opens, and every status change is a locked read-modify-write on that file. The agent's own lifecycle hooks fold the completion into it, which is why `-p` refuses an agent without [installed and trusted hooks](../agents/adapter.md#hook-install). Everything else on this page either produces that record or reacts to it.

`-p` adds no execution engine of its own. It sequences parts the harness already has: the launch path, the budget gates, the message path, and the exec wrapper.

## Module layout

| File | Owns |
| --- | --- |
| [`store/run.rs`](../../../crates/rimz/src/store/run.rs) | The durable record and its wire: `RunRecord`, `RunStatus` and its exit codes, `RunVerify`, the temp-file-plus-rename codec, the `run_completed` wake frame with its sender `wake_run`, and the waiter probe `run_waiter_is_live`. |
| [`harness/run.rs`](../../../crates/rimz/src/harness/run.rs) | Policy over that record: `SupervisedRunRequest`, the locked `create` and status transitions, the lock-free `load` and `list` wrappers, the lifecycle fold, and cancellation. |
| [`harness/run/report.rs`](../../../crates/rimz/src/harness/run/report.rs) | The `joined_at` and `report_message_id` stamps that joins and subagent digests share. |
| [`harness/run_wake.rs`](../../../crates/rimz/src/harness/run_wake.rs) | The receiving half of the blocking wait: the per-run datagram socket, frame validation, the poll loop, and the timeout and cancellation transitions it drives. |
| [`harness/run_timeout.rs`](../../../crates/rimz/src/harness/run_timeout.rs) | Producer-side deadline detection and spawning the detached timeout helper. |
| [`harness/prompt_compose.rs`](../../../crates/rimz/src/harness/prompt_compose.rs) | `retry_prompt` and `verify_reprompt`, beside the system-prompt composition [fleet.md](./fleet.md#system-prompt-composition) owns. |
| [`cli/supervised.rs`](../../../crates/rimz/src/cli/supervised.rs) | Command-neutral effects: agent and program preflight, the pane's exec argv, stop and cancel, the SIGINT cancellation flag, `--timeout` parsing, and the stream-json prompt reader. |
| [`cli/supervised/run.rs`](../../../crates/rimz/src/cli/supervised/run.rs) | The driver `run_supervised` that both `agents -p` and loop fires call: preparation, placement, the attempt loop, the verify loop, and the retry loop. |
| [`cli/supervised/output.rs`](../../../crates/rimz/src/cli/supervised/output.rs) | The output projections: text, JSON, the `StreamSink` for NDJSON `RunStreamEvent`s and streamed text, and the stderr forensics block. |
| [`cli/supervised/stream.rs`](../../../crates/rimz/src/cli/supervised/stream.rs) | Streaming while a run is live, for a blocking caller and for an attached `agents wait --stream`. |
| [`cli/supervised/verify.rs`](../../../crates/rimz/src/cli/supervised/verify.rs) | Running the verify command and delivering its re-prompt. |
| [`cli/supervised/pane.rs`](../../../crates/rimz/src/cli/supervised/pane.rs) | Loop-zone and subagent-zone placement, finding the run's pane, capturing its failure tail, and closing it. |
| [`cli/agents_cmd/exec.rs`](../../../crates/rimz/src/cli/agents_cmd/exec.rs) | The in-pane wrapper's side: recording its pane and provider process, the process-death backstop, and self-cleanup. |
| [`cli/agents_cmd/run_timeout.rs`](../../../crates/rimz/src/cli/agents_cmd/run_timeout.rs) | The hidden `agents run-timeout` helper that settles an overdue run. |
| [`cli/agents_cmd/wait.rs`](../../../crates/rimz/src/cli/agents_cmd/wait.rs), [`stop.rs`](../../../crates/rimz/src/cli/agents_cmd/stop.rs) | Joining on runs started elsewhere, and cancelling them. |

## The record

`RunRecord` carries the run's identity, its launch choices, and everything an inspection surface needs after the pane is gone.

| Group | Fields |
| --- | --- |
| Identity | `run_id`, `workspace_id`, `kind`, `agent_id`, `agent_name`, `pane_id` |
| Provider process | `provider_pid`, `provider_process_start` |
| Provenance | `prompt`, `worktree_path`, `permission_mode`, `budget`, `retry_of`, `loop_task` |
| Retention | `keep`, `subagent` |
| Outcome | `status`, `last_message`, `verify`, `failure_tail`, `transcript_path` |
| Accounting | `cost_usd`, `input_tokens`, `output_tokens` |
| Reporting | `joined_at`, `report_message_id` ([subagents.md](./subagents.md#the-lifecycle-end-to-end)) |
| Timing | `started_at`, `deadline_at`, `updated_at`, `completed_at` |

`agent_id` starts empty. The first lifecycle observation that matches the run fills it, which is how the record binds to a session whose id did not exist when the record was written. `transcript_path` points at the provider's own session file, which streaming reads directly; the RimZ transcript log that `rimz transcript` renders is a different file. The wrapper stamps `provider_pid` and its process-start token when it spawns the provider, so a later signal cannot reach a reused PID.

Records live at `~/.local/state/rimz/workspaces/<id>/runs/<run_id>.json` and stay there until an operator removes state. `store::run` owns the schema and the codec, which publishes through the store's atomic temp-file-plus-rename helper. `harness::run` owns every transition and takes the workspace lock for it; its `create`, `load`, and `list` wrappers let a caller holding `StatePaths` reach the codec without resolving the runs directory. Store reset is the one writer outside `harness::run`: under the same lock, it cancels every active run before rotating room state.

Live agent state stays out of the record. `rimz agents show <run-id>` reads the record and attaches the live card from the snapshot at read time (`harness::run::live_status`), so agent activity costs no locked writes.

## Status and exit codes

`RunStatus` is the state machine, and `RunStatus::exit_code` is the whole caller-facing contract.

| Status | Exit | Reached by |
| --- | --- | --- |
| `Pending` | (124) | the record is written and the agent has not reported yet |
| `Running` | (124) | the first non-terminal lifecycle observation, or a verify re-prompt reopening the run |
| `Completed` | `0` | a root `TurnEnded` that did not error |
| `Failed` | `1` | a root `TurnEnded` that errored, a session `Ended` before any turn result, the wrapper's process-death backstop, or a pane that failed to open |
| `VerifyFailed` | `123` | the verify command stayed red through `--max-attempts` total turns |
| `TimedOut` | `124` | the blocking waiter's `--timeout` elapsed, or the timeout helper found `deadline_at` overdue |
| `BudgetExceeded` | `125` | the budget-park helper stopped the run's agent mid-turn for crossing a dollar cap: its launch `--budget`, the turn cap, or the room or account cap ([budget.md § The park](./budget.md#the-park)) |
| `Canceled` | `130` | `rimz agents stop`, Ctrl+C on a blocking caller, a `TurnInterrupted` signal, a subagent's parent ending, or store reset |

The parenthesized codes are what a caller exits with when it stops waiting on a live run.

Exit `125` also comes from a run that never started. The provider quota gate and `budget::scope_gate` refuse before any record exists, return `SupervisedRunOutcome::BudgetExceeded`, and `run_print` exits `125` straight from that outcome. A script reading run records never sees a gate refusal.

Terminal statuses are absorbing. `RunRecord::mark_terminal` returns false when the record is already terminal, and the transitions pass that on as a `wrote` flag, so each run sends exactly one wake datagram per terminal write. Two transitions leave a terminal status, and both require it to be exactly `Completed`: `reopen_for_verify` moves it back to `Running`, and `verify_failed` moves it to `VerifyFailed`.

## Life of a run

The driver is `run_supervised` in `cli/supervised/run.rs`. One call can hold several attempts, and the order inside an attempt matters.

1. **Prepare.** Resolve the workspace, the caller, and the one-cell layout ([fleet.md § From spec to panes](./fleet.md#from-spec-to-panes)), and reject a prompt that parses as a spec token (`plan::reject_prompt_that_looks_like_spec`), which catches a spec typed where the prompt belongs. Then preflight, before the multiplexer is touched: sandbox capabilities, the adapter's hooks and turn-lifecycle signal (`preflight_hooks` with `TurnLifecycleNeed::Wired`), the provider's recorded trust for the launch directory, and the provider program resolving on `PATH` after shell startup. An adapter with no verified turn-completion signal refuses here.
2. **Birth the room.** `RoomContext` freezes the room's logins, and the provider quota gate runs once. A caller outside a room gets one; an attended caller (stdin is a terminal) may reset a stuck room, while a non-interactive caller gets an error asking for an explicit reset.
3. **Gate on dollars.** At the top of every attempt, the provider quota gate and [`budget::scope_gate`](./budget.md#the-fail-fast-gate) run again. A refusal returns `SupervisedRunOutcome::BudgetExceeded` without writing a record or opening a pane.
4. **Build the identity.** A `RunRecord` is built in memory, with `deadline_at` set to `started_at` plus `--timeout` when given. The store opens a launch batch that mints the provisional agent row and name, and that name is stamped onto the record. On a retry attempt an explicit `--name` becomes a soft name, so the fresh attempt can mint again while the prior ended row keeps its handle.
5. **Bind the waiter.** A blocking run binds its wake socket before the record exists and before the pane opens, so no completion can land in a gap. A `--bg` run binds nothing.
6. **Write the record, then open the pane.** `run::create` persists the record, then the pane opens running the exec wrapper with the run id in its request. Placement follows `run_placement`: a scheduled loop fire splits into the `rimzd` loop panel and falls back to a run tab; `--new-tab` or a caller with no pane gets a new tab; a `rimz subagents` child goes to the [subagent zone](./subagents.md#pane-zones); everything else splits the current tab without taking focus. A pane that fails to open marks the run `Failed` and fails the launch batch.
7. **Wait.** The waiter blocks until the record is terminal. A `--bg` run prints the agent name and returns here.
8. **Verify.** With `--verify` and a `Completed` record, the [verify loop](#verification-re-arms-the-same-run) runs.
9. **Reclaim.** Unless `--keep`, the driver closes the run pane ([Reclaiming the run pane](#reclaiming-the-run-pane)).
10. **Retry or finish.** A `Failed` run with retries left prints its forensics and starts a new attempt at step 3. Anything else returns the record, which the caller projects to stdout and turns into an exit code.

## Completion: folding lifecycle observations

`run::record_lifecycle` folds one agent lifecycle observation into the record. It returns `Some(record)` only when that observation newly made the run terminal, which is what lets the writer send exactly one datagram.

The fold filters before it classifies:

- An observation carrying a `parent_agent_id`, or a `SubagentStarted` or `SubagentStopped` signal, is dropped. A child's completion never finishes its parent.
- An observation of a different `kind`, or one arriving after the record is terminal, is ignored.
- Session binding is strict once made. An unbound record adopts the observed `agent_id` and name. A bound record ignores an observation for a different session id and an observation with no session id, so a same-kind descendant with its own session cannot end the run.

What passes is classified by `LifecycleSignal::terminal_disposition` in `agents::lifecycle`, and `fold_lifecycle` maps the disposition to a status:

| Lifecycle signal | Result |
| --- | --- |
| `TurnEnded { errored: false, parked_on_background: false }` | `Completed` |
| `TurnEnded { errored: true, parked_on_background: false }` | `Failed` |
| `TurnEnded { parked_on_background: true }` | not terminal: the agent parked on background work and ends the turn later |
| `TurnInterrupted` | `Canceled` |
| `Ended` | `Failed`: the session ended without reporting a turn result |
| anything else | not terminal: promotes `Pending` to `Running` and records the first transcript path |

A terminal fold also stores the transcript path and the final assistant message. `record_assistant_message` lets an adapter store its declared final output earlier without ending the run.

Process death is a backstop and never reads as success. When the provider process exits, the wrapper waits `RUN_EXIT_TERMINAL_GRACE` (500 ms) for a terminal record. If none lands, it captures its own pane tail, writes `Failed` through `fail_if_nonterminal`, and wakes the waiter. A provider that exits cleanly without a terminal hook is still a failed run.

## The wake socket

`run_wake::RunWaiter` exists only to cut the latency of a polling loop that is already correct.

The waiter binds `sock/run.<short_id>.sock` in the runtime directory and keeps it bound across verify re-prompts. Whoever newly writes a terminal transition calls `store::run::wake_run`, which sends a `run_completed` datagram to that path if the socket file exists. The waiter checks every frame against `(workspace_id, run_id)`, logs and drops a mismatch or an unparseable frame, and keeps receiving.

The record on disk stays the truth. `wait_terminal` reloads it every `RUN_WAIT_POLL` (250 ms) whether or not a datagram arrived, so a lost, late, or mismatched datagram costs at most one tick. Streamed and plain callers use the same `wait_terminal` call; a streaming caller passes an observer closure that sees each reloaded record.

The waiter also writes two transitions itself. When the SIGINT flag from `install_run_interrupt_flag` is set, it calls `run::cancel_and_wake`, which wakes only on a newly written cancellation. When the caller's `--timeout` elapses, it writes `TimedOut`.

A second path enforces `deadline_at` without any waiter. On its heavy-lane refresh, the elected sidebar producer calls `run_timeout::enforce`, which lists run records and spawns the hidden `agents run-timeout` helper for each `Pending` or `Running` record past its deadline. This covers background runs, which have no waiter, and blocking runs whose caller died. The helper rechecks the deadline under the workspace lock (`timeout_if_due`), writes `TimedOut`, wakes any waiter, sends `SIGTERM` to a subagent's recorded provider process when the PID and start token still match, and reclaims the pane unless the run is a kept subagent. Detection stays read-only in the sidebar process; the short-lived helper owns every mutation.

`deadline_at` is fixed when the attempt starts, and a verify re-prompt does not move it, so the helper can time out a run in the middle of its verify rounds even though each blocking wait gets the full `--timeout`.

## Verification re-arms the same run

`--verify <CMD>` runs a shell command in the run's working directory after a completed turn. A red result reuses the same agent session instead of starting a new one.

```text
turn completes
  └─ run verify command in the run cwd
       ├─ passes ─────────► verify_passed; record stays `completed`
       ├─ attempt == max ─► verify_failed; record becomes `verify_failed` (exit 123)
       └─ red, attempts left
            ├─ reopen_for_verify: `completed` → `running`, evidence stored
            ├─ deliver the verify_reprompt through message::deliver::nudge_now
            │    into the same pane and the same agent session
            └─ wait on the same bound socket for the next root TurnEnded
```

No provider resume and no replacement pane enter this path, so the next `TurnEnded` makes the same record terminal again and wakes the same socket. `--max-attempts` counts total agent turns, defaults to `3`, and must be at least `1`. The verify command runs under `--timeout` when set and `CHECK_DEFAULT_TIMEOUT` (5 minutes) otherwise, and a timed-out verify is red. The re-prompt carries the command, its exit status, and a 4 KiB output tail, and it goes through the [message path](./messaging.md) with gate `Any`. A re-prompt that is queued but not delivered fails the run. A cancellation observed during verification stores the evidence, then cancels.

`--retries <N>` is a separate loop. It reruns only a `Failed` run (`RunStatus::is_retryable`) in a fresh session and pane, appending the previous attempt's captured pane tail to the original prompt inside a `<previous-attempt-failure>` block. Each attempt writes its own record with `retry_of` pointing at its predecessor, and the last attempt decides the exit code. Timeouts, budget stops, and cancellations are never retried. The two loops compose: a verify repair stays in-session after a completed turn, while a retry starts a fresh session with the verify attempt count reset.

Both loops need a blocking run, so both refuse `--bg` and `--output-format stream-json`. The `--bg` conflict is declared on the clap arguments (`conflicts_with = "bg"`); the stream-json refusals and the `--max-attempts` checks live in `validate_supervised_output`. The user gets an error at the entry point, because the background path returns before the verify phase and before any retry could fire.

## Output and input projections

`--output-format` chooses what a blocking caller prints. All three formats read the same record.

| Format | Prints |
| --- | --- |
| `text` (default) | the final assistant message on stdout, and nothing else |
| `json` | the full run record, pretty-printed |
| `stream-json` | NDJSON `RunStreamEvent`s as the turn runs: `message` per new assistant message, `status` when the live card changes, and one `end` with the status and last message |

Text output keeps stdout as the answer channel. A run that did not complete prints a stderr forensics block (`print_run_forensics`): the status and exit code, the captured pane tail, the failed verify result, and the transcript path. A completed run with no extracted message prints a one-line stderr note instead.

`--input-format` chooses the prompt source. `text` takes the positional prompt, followed by `--stdin` content inside `<stdin>` tags when both are given. `stream-json` reads `{"type":"user"}` messages from stdin until EOF and joins their text (`read_stream_json_prompt`); it refuses a positional prompt and `--stdin`.

Streaming reads the provider's transcript at `record.transcript_path`. Both `--output-format stream-json` and `agents wait --stream` hold an `agents::transcript::TranscriptCursor`, which asks the adapter's `read_assistant_transcript_page` for assistant messages appended since its position. The cursor is keyed by path and session id, restarts when either changes, and rewinds when the source shrinks below it. A blocking caller streams from the wake loop's observer; `agents wait --stream` polls every 500 ms instead (`stream_attached_run`). `agents wait --stream --json` emits the same NDJSON events, and plain `--stream` renders assistant text. The wake socket plays no part in streaming.

## Launch options on `-p`

A run launches through the same layout resolution and finalization as an interactive launch, so model, effort, profile `auto-compact`, system-prompt files, and the adapter's refusal of a setting it cannot render all behave as [fleet.md § From spec to panes](./fleet.md#from-spec-to-panes) describes. Four things are specific to `-p`:

- The permission posture is `auto` unless `--ask` or `--yolo` is given; the two flags conflict. A virtual cell's mode overrides the flag, so `rimz agents claude-plan ... -p` runs in plan mode although `-p` has no `--plan` flag.
- `--max-turns` renders through the adapter's turn-limit argv, and an adapter without one refuses it at finalization.
- The layout must resolve to exactly one agent cell and no other cells.
- `--fresh` and `--json` are refused; JSON output is `--output-format json`.

The per-kind argv mappings live in the adapter pages indexed under [the agent layer](../README.md#the-agent-layer).

## Background runs and joining

`--bg` splits starting a run from waiting on it. The driver opens the pane, prints the agent name, and returns without binding a waiter. `rimz agents wait <ref>...` then polls the durable records every 500 ms, settling when all targets finish or, with `--any`, when the first one does. A reference is the printed name, a run id, or any [agent address](./fleet.md#the-address), so one handle works for `wait`, `show`, `stop`, and `message`. A reference that resolves to a live agent whose newest run is already terminal waits on the agent instead, and settles when it reaches its done gate or fails (`resolve_wait_target`).

Because the record is the run, a `wait` in another shell or a later CI step sees the same outcome the launching process would have seen. The wake socket is per host; only the polled record is shared.

`wait` never owns lifecycle transitions. It maps a settled status to an exit code, and with several targets exits with the first non-completed target's code. It may stamp `joined_at` when it prints a result to an attended caller, and cancel a queued subagent digest whose every row has been joined ([subagents.md § The lifecycle, end to end](./subagents.md#the-lifecycle-end-to-end)). It never signals a process, cancels a run, or closes a pane.

Three consequences follow from that:

- Never calling `wait` changes nothing about a run's lifecycle, because background [pane reclamation](#reclaiming-the-run-pane) belongs to the in-pane wrapper.
- `--any` returns the first finisher and leaves the others running to their own completion, which is why the guide's [race recipe](../../guide/scripting.md#in-a-pipeline) stops the loser explicitly.
- `wait --timeout` bounds the caller's patience, not the run. It exits `124` with the run still live, the same code a `TimedOut` run exits with. With several targets or `--any`, `--json` stamps each unfinished target `timed_out` in its result map; a single target prints nothing. Only the record tells them apart (a run the caller stopped waiting on is still `running`), so a script that needs the difference rereads it with `agents show`.

## Reclaiming the run pane

Cleanup is best-effort and split by who is still alive to do it.

A blocking run's driver closes the pane after the verify phase unless `--keep`. First, `record_failure_tail_before_cleanup` captures a pane tail for a non-completed record that has none; `record_failure_tail` keeps the first tail written, so a tail the wrapper captured as it died is never overwritten. `close_run_pane` closes the recorded `pane_id`, and falls back to the agent row found by `(kind, agent_id)` in the snapshot when no pane was recorded or closing it failed. A `Canceled` record goes through the stop backstop below instead.

The in-pane wrapper reclaims its own pane when the request set `exit_on_run_completion`: every `--bg` run, every `rimz subagents` child, and every loop-owned run, unless `--keep`. `supervise_child` rereads the record every `RUN_MONITOR_POLL` (250 ms). Once the record is terminal and no waiter socket is live (`run_waiter_is_live`), it sends the provider `SIGTERM`, escalates to `SIGKILL` after `CHILD_SIGNAL_GRACE` (300 ms), runs worktree cleanup for a marked worktree, and closes its own pane. The live-waiter check leaves a blocking caller in charge of verification and cleanup; the wrapper takes over only when that caller is gone. The close depends on the record alone, never on whoever launched or joins the run.

A stop, or a Ctrl+C that cancels a blocking caller, writes `Canceled` through `cancel_and_wake` and then runs `close_stopped_run_pane_after_grace`: if the run's pane is still listed after `STOP_BACKSTOP_GRACE` (3 seconds), it closes it. A stop on a terminal `--keep` record leaves the status alone and only reclaims the pane, whether the reference was the run id or the agent name. The timeout helper ends with the same backstop, so a wedged wrapper cannot leave an overdue pane behind.

A `rimz subagents` child adds its own rules: `--keep` holds the pane past completion and parent exit, a parent watchdog cancels the run when the parent's launch ends, and the producer's orphan scan backstops missed digests and orphans. [subagents.md § The lifecycle, end to end](./subagents.md#the-lifecycle-end-to-end) owns them.

Worktree cleanup depends on retries. A run without `--retries` marks its pane for wrapper-side worktree cleanup. A run with `--retries` leaves the mark off and has the driver clean up after the final attempt, so an intermediate failure does not remove the tree the next attempt needs.

## Runs the scheduler starts

A loop fire calls the same `run_supervised` driver with a request shaped by `schedule::runner::shape_loop_owned`: `loop_task` names the task so its records can be found, a scheduled fire without a task timeout takes the configured default, and wrapper self-cleanup is on unless the task keeps its pane. A scheduled fire also sets `loop_zone`, which places the pane in the `rimzd` loop panel. A prompted `rimz agents` launch made by a loop check converts into the same loop-owned request. A budget refusal becomes one history row instead of an exit code. The scheduler's side is [loops.md](./loops.md).

## See also

- [fleet.md](./fleet.md): the launch, address, and cleanup machinery a run rides on.
- [subagents.md](./subagents.md): what a `rimz subagents` child adds to a run.
- [loops.md](./loops.md): driving these runs on a clock.
- [messaging.md](./messaging.md): the delivery path a verify re-prompt uses.
- [budget.md](./budget.md): the gates and the park behind exit `125`.
- [store.md](../store.md): where run records sit among the other durable state.
- [cli/agents.md](../../reference/cli/agents.md#supervised-runs--p): every flag on `-p`, `wait`, `show`, and `stop`.
