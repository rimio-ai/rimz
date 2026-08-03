# Supervised runs

> One supervised turn end to end: the durable record, the completion signal, the wait, verification and retry, the output projections, and pane reclamation. [fleet.md](./fleet.md) is the map for this area and owns the launch machinery a run rides on. For users, the guide is [scripting.md](../../guide/scripting.md) and the flag reference is [cli/agents.md](../../reference/cli/agents.md#supervised-runs--p).

## What a supervised run is

`rimz agents <spec> <prompt> -p` gives a caller the contract of `claude -p`: one prompt in, one answer on stdout, one exit code to branch on. What makes it a *supervised* run rather than a headless one is that the turn happens in a real pane, in the caller's room, as a first-class fleet member with a card, a handle, and a transcript.

That choice creates the central problem. A headless process tells you it finished by exiting. An agent in a pane does not: the CLI stays alive after the turn, the pane keeps a shell, and the process can outlive the work or die before reporting it. So a run needs an external completion signal, and it needs to survive the caller crashing, the pane closing, and the machine rebooting.

The rule that resolves it:

> **The durable run record is the run. The pane, the wrapper, and the wake socket are all latency.**

A `RunRecord` is written under `~/.local/state/rimz/workspaces/<id>/runs/<run_id>.json` before the pane opens and stays there until an operator clears state. Every status change is a locked read-modify-write on that file. The completion signal is the agent's own lifecycle hooks folding an observation into it, which is why [installed and trusted hooks](../agents/adapter.md#hook-install) are the one hard prerequisite: a run with no hooks has no way to learn its turn ended. Everything else in this doc is either producing that record or reacting to it.

`-p` adds no execution engine of its own. It sequences pieces the harness already has: the budget gate, the launch path, the message path, and the wrapper.

## Module layout

| File | Owns |
| --- | --- |
| [`harness/run.rs`](../../../crates/rimz/src/harness/run.rs) | The vocabulary and durable record interface: `SupervisedRunRequest`, `RunRecord`, `RunStatus` and its exit codes, `RunVerify`, locked create/read/update intents, the lifecycle fold, cancellation, and the retry and verify prompt builders. |
| [`harness/run_timeout.rs`](../../../crates/rimz/src/harness/run_timeout.rs) | Producer-side deadline detection and detached timeout-helper spawning. |
| [`harness/run_wake.rs`](../../../crates/rimz/src/harness/run_wake.rs) | The blocking wait: the per-run datagram socket, frame validation, the poll loop, and timeout and cancellation transitions. |
| [`cli/supervised/run.rs`](../../../crates/rimz/src/cli/supervised/run.rs) | The driver both `agents -p` and loop fires call: preparation, placement, the attempt loop, the verify loop, and the retry loop. |
| [`cli/supervised/output.rs`](../../../crates/rimz/src/cli/supervised/output.rs) | The output projections: text, JSON, the NDJSON `RunStreamEvent` sink, and the stderr forensics block. |
| [`cli/supervised/stream.rs`](../../../crates/rimz/src/cli/supervised/stream.rs) | Streaming while a run is live, for both a blocking caller and an attached `agents wait --stream`. |
| [`cli/supervised/verify.rs`](../../../crates/rimz/src/cli/supervised/verify.rs) | Running the verify command and delivering its re-prompt. |
| [`cli/supervised/pane.rs`](../../../crates/rimz/src/cli/supervised/pane.rs) | Finding the run's pane, capturing its failure tail, and closing it. |
| [`cli/agents_cmd/exec.rs`](../../../crates/rimz/src/cli/agents_cmd/exec.rs) | The in-pane wrapper's side: recording its own pane, the process-death backstop, and background self-cleanup. |
| [`cli/agents_cmd/wait.rs`](../../../crates/rimz/src/cli/agents_cmd/wait.rs), [`stop.rs`](../../../crates/rimz/src/cli/agents_cmd/stop.rs) | Joining on runs started elsewhere, and cancelling them. |

## The record

`RunRecord` carries the run's identity, its launch choices, and everything an inspection surface needs after the pane is gone.

| Group | Fields |
| --- | --- |
| Identity | `run_id`, `workspace_id`, `kind`, `agent_id`, `agent_name`, `pane_id` |
| Provenance | `prompt`, `worktree_path`, `permission_mode`, `budget`, `retry_of`, `loop_task` |
| Outcome | `status`, `last_message`, `verify`, `failure_tail`, `transcript_path` |
| Accounting | `cost_usd`, `input_tokens`, `output_tokens` |
| Timing | `started_at`, `deadline_at`, `updated_at`, `completed_at` |

Two fields are worth calling out. `transcript_path` points at the *provider's own* session file, not the RimZ transcript log that `rimz transcript` renders; streaming reads that file directly. `agent_id` starts empty and is filled by the first matching lifecycle observation, which is how the record binds to a session it did not know the id of when it was written.

Records are cold-path durable state. `harness::run` owns their schema, transitions, and workspace-lock placement; its private store codec performs temp-file-plus-rename through the store atomic helpers. Store reset alone writes through that codec directly, under the same workspace lock, so it can cancel active runs before rotating room state. Records remain until an operator removes state. Live fields deliberately stay out of the record: `rimz agents show <run-id>` reads the retained record and attaches live card context from the snapshot at read time, so agent drift creates no extra locked writes.

## Status and exit codes

`RunStatus` is the state machine, and its `exit_code` is the whole caller-facing contract.

| Status | Exit | Reached by |
| --- | --- | --- |
| `Pending` | (124) | the record is written, the pane has not reported yet |
| `Running` | (124) | the first non-terminal lifecycle observation, or a verify re-prompt reopening the run |
| `Completed` | `0` | a root `TurnEnded` that did not error |
| `Failed` | `1` | a root `TurnEnded` that errored, a session `Ended` before any turn result, or the wrapper's process-death backstop |
| `VerifyFailed` | `123` | the verify command stayed red through `--max-attempts` total turns |
| `TimedOut` | `124` | the blocking waiter elapsed, or the producer found a durable deadline overdue |
| `BudgetExceeded` | `125` | a scope cap or an exact managed-launch provider quota refused the run |
| `Canceled` | `130` | `rimz agents stop`, Ctrl+C on a blocking caller, or a `TurnInterrupted` signal |

Terminal statuses are absorbing. `mark_terminal` returns a `wrote` flag that is false when the record was already terminal, and callers use that flag to send exactly one wakeup datagram per run. Only one transition breaks absorption, `reopen_for_verify`, and it requires the current status to be exactly `Completed`.

## Life of a run

The driver is `cli::supervised::run_supervised`. One call can contain several attempts, and the order inside one attempt matters.

1. **Prepare.** Resolve the workspace and machine config, resolve and finalize the one-cell layout ([fleet.md § From spec to panes](./fleet.md#from-spec-to-panes)), and reject a comma-bearing prompt that was probably a mistyped spec.
2. **Gate on dollars.** The provider-account quota gate and [`budget::scope_gate`](./budget.md#the-fail-fast-gate) run before the room is touched, and again at the top of every attempt. A refusal returns `SupervisedRunOutcome::BudgetExceeded` and never writes a record or opens a pane, which is why exit `125` is distinguishable from a run that started and then hit a cap.
3. **Birth the room.** A caller outside a room gets one; an attended caller may reset a stuck room, while a non-interactive caller requires an explicit reset instead of silently resetting under a script.
4. **Build the identity.** A `RunRecord` is constructed in memory, including a `deadline_at` when the request carries `--timeout`; the store opens a launch batch that mints the provisional agent row and name, and that name is stamped onto the record. On a retry attempt an explicit `--name` is downgraded to a soft name, so the fresh attempt can remint while the prior ended row keeps its handle.
5. **Bind the waiter.** A blocking run binds its wake socket *before* the record exists and before the pane opens, so no completion can land in a gap. A `--bg` run binds nothing.
6. **Write the record, then open the pane.** `run::create` persists the record, then the pane opens: a split of the current tab by default, a new tab when forced or when there is no ambient pane, and the locked `rimzd` loop zone for scheduled fires. The pane runs the exec wrapper with `RIMZ_RUN_ID` exported.
7. **Wait.** The waiter blocks until the record is terminal. `--bg` prints the agent name and returns here.
8. **Verify.** If `--verify` is set and the run completed, the [verify loop](#verification-re-arms-the-same-run) runs.
9. **Reclaim.** Unless `--keep`, the pane closes. A `rimz subagents` wrapper is the exception: it stops the provider but retains the pane until explicit stop or parent exit.
10. **Retry or finish.** A `Failed` run with retries left starts a new attempt with an augmented prompt. Anything else returns the record, which the caller projects to stdout and turns into an exit code.

## Completion: folding lifecycle observations

The run learns it finished from `run::record_lifecycle`, which folds one agent lifecycle observation into the record and returns `Some(record)` only when the observation *newly* made it terminal. That single-writer property is what lets exactly one datagram be sent.

The fold rejects before it accepts.

- An observation carrying a `parent_agent_id`, or a `SubagentStarted` or `SubagentStopped` signal, is dropped outright. A child completion must never finish its parent.
- A different `kind`, or an already-terminal record, is ignored.
- Session binding is strict once made. An unbound record adopts the observed `agent_id` and name; a bound record ignores any observation for a different session id, and ignores an observation with no session id at all. A same-kind descendant with its own session therefore cannot end the parent run.

What survives that filter maps through `terminal_status_for_signal`:

| Lifecycle signal | Result |
| --- | --- |
| `TurnEnded { errored: false, parked_on_background: false }` | `Completed` |
| `TurnEnded { errored: true, parked_on_background: false }` | `Failed` |
| `TurnEnded { parked_on_background: true }` | not terminal: the agent parked on background work and will end the turn later |
| `TurnInterrupted` | `Canceled` |
| `Ended` | `Failed` (a session that ended without ever reporting a turn result) |
| anything else | not terminal: promotes `Pending` to `Running`, and records the first transcript path |

**Process death is the backstop, never the success path.** When the wrapper sees the agent process exit and no terminal hook lands within a short grace, it captures its own pane tail, writes `Failed`, and wakes the waiter. A pane that exits cleanly is never read as a completed turn.

## The wake socket

`run_wake::RunWaiter` exists purely to cut latency on an otherwise correct polling loop.

The waiter binds `sock/run.<short_id>.sock` before the pane opens and stays bound across verification re-prompts. When a terminal transition is newly written, the writer sends a `run_completed` datagram to that path. The waiter validates every frame by `(workspace_id, run_id)`, logs and drops a mismatch, and keeps receiving.

The record on disk stays truth. The waiter polls it every 250 ms regardless, so a lost, delayed, or mismatched datagram costs at most one tick. A blocking waiter can write the `--timeout` transition itself; a background run has no waiter, so the elected sidebar producer scans durable run records on its normal heavy-lane cadence. When `deadline_at` is due it spawns the hidden `agents run-timeout` helper. That helper rechecks the deadline under the workspace lock, writes `TimedOut`, wakes any waiter, and reclaims the pane. Detection remains read-only in the sidebar process; the short-lived helper owns every mutation.

The waiter also owns the caller's cancellation transition, which the CLI's signal handler sets. `run::cancel_and_wake` couples a newly written cancellation to its wake, and leaves an already-terminal `--keep` record untouched when a stop is only reclaiming a pane.

Streamed and non-streamed callers use the same `wait_terminal` call and differ only by an optional observer closure.

## Verification re-arms the same run

`--verify` runs a shell command in the run's working directory after a completed turn, and the interesting design choice is that a red result reuses the same agent session rather than starting a new one.

```text
turn completes
  └─ run verify command in the run cwd
       ├─ passes ─────────► verify_passed; record stays `completed`
       ├─ attempt == max ─► verify_failed; record becomes `verify_failed` (exit 123)
       └─ red, attempts left
            ├─ reopen_for_verify: `completed` → `running`, evidence stored
            ├─ deliver the evidence prompt through the durable message path
            │    into the same pane and the same agent session
            └─ wait on the same bound socket for the next root TurnEnded
```

Because no provider resume and no replacement pane enters the path, the next `TurnEnded` makes the same record terminal again and sends another datagram to the same socket. `--max-attempts` counts total agent turns and defaults to `3`. The verify command uses `--timeout` when set and a five-minute cap otherwise; a timed-out verify is red. A cancellation observed mid-verify stores the evidence, then cancels.

`--retries` is a different loop and worth keeping distinct. It reruns a **failed** run (`RunStatus::Failed` only) in a **fresh session**, appending the previous attempt's captured pane tail to the original prompt inside a `<previous-attempt-failure>` block. Each attempt writes its own record with `retry_of` pointing at its predecessor, and the last attempt decides the command's exit code. Timeouts, budget stops, and cancels stay terminal and are never retried. The two compose: verification repairs stay in-session after a completed turn, while a retry starts a fresh session and resets the verify attempt count.

Both flags need a blocking run to have anything to act on, so both refuse `--bg` and `--output-format stream-json`. The `--bg` conflict is declared on the arguments themselves (`conflicts_with = "bg"`), so clap rejects it before `run_supervised` is reached; the `stream-json` refusals live in `validate_supervised_output`. Either way the user gets an error rather than a flag that quietly does nothing, which matters because the background path returns before the verify phase and before any retry could fire.

## Output and input projections

`--output-format` chooses what a blocking caller prints, and all three read the same record.

| Format | Prints |
| --- | --- |
| `text` (default) | the final assistant message on stdout, and nothing else |
| `json` | the full run record, pretty-printed |
| `stream-json` | NDJSON `RunStreamEvent`s (`message`, `status`, `end`) as the turn runs |

Text output keeps stdout as the answer channel. A failed, timed-out, or canceled run prints a stderr forensics block with the status, the captured pane tail when present, and the transcript path, so the happy path stays pipeable.

`--input-format` chooses the prompt source: `text` takes the positional prompt, folding explicit `--stdin` content into `<stdin>` tags when both are present, while `stream-json` reads user messages from stdin until EOF.

**Streaming is transcript-tail based.** Both `--output-format stream-json` and `agents wait --stream` poll the *provider's* transcript at `record.transcript_path` with the torn-write-safe cursor used for transcript reads, parse only newly appended assistant messages through the selected adapter's `parse_transcript_messages`, and reset the cursor if the path changes. `agents wait --stream --json` emits the same NDJSON events; plain `--stream` renders assistant text. The wake socket is not part of streaming; it only wakes a blocking producer promptly.

Long-running commands animate their phase and elapsed time on an interactive stderr terminal. `RIMZ_NO_PROGRESS=1` disables it everywhere, and non-TTY stderr and RimZ-launched agent shells carrying `RIMZ_AGENT_KIND` disable it automatically.

## Adapter-owned launch surface

A run's permission posture is `auto` by default, `--ask`, or `--yolo`; those two flags are mutually exclusive. A virtual cell in the spec overrides the flag, so `rimz agents claude-plan ... -p` runs in plan mode even though `-p` exposes no `--plan` flag. Model and effort render through each adapter's `render_preset`. The resolved system-prompt file path stays typed through planning and is mapped directly to the adapter's argv or environment during provider-process compilation.

After profile and CLI arguments merge, launch reconciles adapter-declared model, effort, and matching system-prompt replacement flags against raw `args`, adopts an args-only model as identity, and stamps an adapter default only when no model was selected. Prompt-file existence and replacement support validate before a pane opens. An adapter with no native rendering for a typed parameter **refuses the launch and names the unsupported setting** rather than dropping the intent. Supervised `--max-turns` renders through a separate per-adapter turn-limit hook. The per-kind mappings live in the adapter docs indexed under [the agent layer](../README.md#the-agent-layer).

An adapter with no verified turn-completion signal refuses `-p` before opening a pane, rather than opening a pane that can never complete.

## Background runs and joining

`--bg` splits starting a run from waiting on it. The driver opens the pane, prints the agent name, and returns without binding a waiter. `rimz agents wait <ref>...` then polls the durable records of one or several runs, joining with `JoinMode::All` or, with `--any`, settling on the first to finish. A reference is the printed name, a run id, or any [agent address](./fleet.md#the-address), so one handle threads through `wait`, `show`, `stop`, and `message`.

Because the record is the run, a `wait` in a different shell, a later CI step, or another machine sharing the state directory sees exactly what the launching process would have seen.

**`wait` only reads.** It polls records and maps a settled status to an exit code; it never signals a process, cancels a run, or closes a pane. Three consequences follow, and each one surprises somebody. Not waiting at all changes nothing about a run's lifecycle, because [pane reclamation](#reclaiming-the-run-pane) belongs to the in-pane wrapper rather than to the joining caller. `--any` returns the first finisher and leaves every loser running to its own completion, which is why the [race recipe](../../guide/scripting.md#in-a-pipeline) stops the loser explicitly. And `wait --timeout` bounds *the caller's patience*, not the run: it exits `124` with the run still live, which is the same code a genuinely `TimedOut` run exits with. `--json` does not disambiguate them either, because the join stamps every unfinished target `timed_out` in its result map on the way out. Only the durable record still knows — a run the caller merely stopped waiting on is still `Running` — so a script that needs the difference re-reads the record with `agents show` rather than trusting the join's exit code.

## Reclaiming the run pane

Cleanup is best-effort and split by who is still alive to do it.

**A blocking run** closes the recorded launch pane after the driver finishes, falling back to finding the agent row by `(kind, agent_id)` in the snapshot when no pane id was recorded. Before that close, a non-completed record with no failure tail yet gets one captured from the pane. First writer wins: a wrapper that already captured a tail as it died cannot be overwritten by later cleanup.

**A background run** hands normal completion reclamation to the in-pane wrapper. Unless `--keep`, the wrapper watches the run record, terminates the agent once it is terminal, runs marked-worktree cleanup, and closes its own pane. Concretely, `supervise_child` re-reads the record every 250 ms, sends `SIGTERM` on the first terminal status, escalates to `SIGKILL` 300 ms later, and then settles: the close is self-initiated by the pane's own wrapper, on the record alone, with no involvement from whoever launched it or joins it. A producer-enforced timeout additionally runs the stop backstop from its hidden helper, so a wedged ordinary wrapper cannot leave the overdue pane behind.

**A subagent background run** stops its provider at the same terminal transition but keeps the wrapper alive, so tmux and Zellij both retain the pane. On spawn the wrapper persists the provider PID and process-start token on the run record. Its timeout helper writes and wakes the terminal record and signals exactly that still-identical provider, without running the ordinary pane-close backstop. The pane closes only when `rimz subagents stop` removes it or its parent ends; `--keep` also disables the parent-death close. The child wrapper owns the fast parent watchdog, while the producer's ten-minute durable orphan sweep is a warning-emitting repair path, not normal reclamation.

**A stop or a Ctrl+C-canceled blocking caller** uses the same terminal record and wake path, then closes the recorded pane if it lingers past a short grace. That reclaims a kept run's pane whether the reference given was the run id or the agent name.

Worktree cleanup interacts with retries: a single-attempt run marks its pane for wrapper-side worktree cleanup, while a run with `--retries` defers cleanup to the driver after the final attempt, so an intermediate failure does not reclaim the tree the next attempt needs.

## Runs the scheduler starts

A loop fire calls the same `run_supervised` driver with a `SupervisedRunRequest` built by the task runner, and differs in three ways: the request carries `loop_task` so the record can be found by task name, `loop_zone` targets the locked `rimzd` loop panel instead of splitting beside a caller, and a typed budget refusal is recorded as one history row rather than mapped to an exit code. The rest of this document applies unchanged. See [loops.md](./loops.md).

## See also

- [fleet.md](./fleet.md): the launch, address, and cleanup machinery a run rides on.
- [loops.md](./loops.md): driving these runs on a clock.
- [messaging.md](./messaging.md): the delivery path a verify re-prompt uses.
- [store.md](../store.md): where run records sit among the other durable state.
- [cli/agents.md](../../reference/cli/agents.md#supervised-runs--p): every flag on `-p`, `wait`, `show`, and `stop`.
