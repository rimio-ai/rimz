use super::*;
use crate::cli::{render, supervised};
use rimz::agents::transcript::TranscriptCursor;
use rimz::harness::run::{SupervisedRunOutcome, SupervisedRunRequest, VerifyStep};
use rimz::harness::run_wake;
use rimz::harness::schedule::runner::CheckSignalGuard;
use rimz::store::run::{RunRecord, RunStatus};
use std::io::{IsTerminal as _, Write as _};
use std::sync::{Arc, Mutex};

pub(in crate::cli) fn run_print(
    request: SupervisedRunRequest,
    presentation: SupervisedPresentation,
    globals: &GlobalFlags,
) -> Result<Option<RunRecord>> {
    let output_format = presentation.output_format;
    let report_to = request.report_to;
    let Some(outcome) = run_supervised(request, presentation, globals, &mut None)? else {
        return Ok(None);
    };
    let record = match outcome {
        SupervisedRunOutcome::Record(record) => Some(*record),
        SupervisedRunOutcome::Background {
            agent_name,
            response_path,
            ..
        } => {
            writeln!(render::out(), "{agent_name}")?;
            supervised::output::write_background_receipt(
                &mut render::err(),
                &[agent_name.as_str()],
                response_path.as_deref(),
                false,
                report_to,
            )?;
            None
        }
        SupervisedRunOutcome::BudgetExceeded { reason } => {
            render::report(&anyhow::anyhow!(reason));
            std::process::exit(RunStatus::BudgetExceeded.exit_code());
        }
    };
    let Some(record_ref) = record.as_ref() else {
        return Ok(record);
    };
    match output_format {
        OutputFormat::Text => {
            let mut stdout = render::out();
            let mut stderr = render::err();
            supervised::output::print_run_output(
                record_ref,
                &mut stdout,
                &mut stderr,
                render::prose::Prose::for_stdout(),
                render::prose::prose_width(0),
            )?
        }
        OutputFormat::Json => crate::cli::render::json_pretty(record_ref)?,
        // stream-json already emitted its events as the run progressed.
        OutputFormat::StreamJson => {}
    }
    Ok(record)
}

struct AttemptContext {
    workspace: rimz::ResolvedWorkspace,
    store: rimz::Store,
    adapter: &'static AgentDefinition,
    cwd: std::path::PathBuf,
    owns_checkout_lifecycle: bool,
    output_format: OutputFormat,
    stream_text: bool,
}

struct PresentationWaiter {
    waiter: run_wake::RunWaiter,
    stream_cursor: Option<TranscriptCursor>,
}

impl PresentationWaiter {
    /// Block until the run reaches a terminal record, streaming transcript
    /// output when the run was started with a stream cursor.
    fn await_terminal(
        &mut self,
        prepared: &AttemptContext,
        room: &rimz::room::RoomContext,
        request: &SupervisedRunRequest,
    ) -> Result<RunRecord> {
        let record = if prepared.output_format == OutputFormat::StreamJson {
            let mut stdout = std::io::stdout().lock();
            let mut sink = supervised::output::StreamSink::ndjson(&mut stdout);
            supervised::stream::stream_blocking_run(
                &self.waiter,
                &prepared.store,
                prepared.adapter,
                request.timeout,
                (
                    self.stream_cursor
                        .as_mut()
                        .context("stream run lost its transcript cursor")?,
                    &mut sink,
                ),
            )?
        } else if prepared.stream_text {
            let mut stdout = render::out();
            let mut gutter = render::GutterWriter::new(&mut stdout);
            let mut stderr = render::err();
            let mut sink = supervised::output::StreamSink::text(
                &mut gutter,
                &mut stderr,
                render::prose::Prose::for_stdout(),
                render::prose::prose_width(4),
            );
            supervised::stream::stream_blocking_run(
                &self.waiter,
                &prepared.store,
                prepared.adapter,
                request.timeout,
                (
                    self.stream_cursor
                        .as_mut()
                        .context("stream run lost its transcript cursor")?,
                    &mut sink,
                ),
            )?
        } else {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .enable_time()
                .build()
                .context("creating run wait runtime")?;
            runtime.block_on(
                self.waiter
                    .wait_terminal(&prepared.store, request.timeout, None),
            )?
        };
        Ok(record_failure_tail_before_cleanup(
            room.backend(),
            &prepared.store,
            &prepared.workspace.session_name,
            record,
        ))
    }
}

struct BlockingAttempt {
    record: RunRecord,
    waiter: PresentationWaiter,
}

fn verify_phase(
    prepared: &AttemptContext,
    room: &rimz::room::RoomContext,
    request: &SupervisedRunRequest,
    blocking: BlockingAttempt,
) -> Result<(RunRecord, Option<anyhow::Error>, PresentationWaiter)> {
    let BlockingAttempt {
        mut record,
        mut waiter,
    } = blocking;
    let Some(cmd) = request.verify.as_deref() else {
        return Ok((record, None, waiter));
    };
    if record.status != RunStatus::Completed {
        return Ok((record, None, waiter));
    }
    let max_attempts = request
        .max_attempts
        .unwrap_or(rimz::harness::run::VERIFY_MAX_ATTEMPTS_DEFAULT);
    let verify_timeout = request
        .timeout
        .unwrap_or(rimz::harness::schedule::runner::CHECK_DEFAULT_TIMEOUT);
    let mut verify_attempt = 1;
    let mut verify_error = None;
    while record.status == RunStatus::Completed {
        let outcome = match supervised::verify::run_verify(&prepared.cwd, cmd, verify_timeout) {
            Ok(outcome) => outcome,
            Err(err) => {
                verify_error = Some(err);
                break;
            }
        };
        let detail = rimz::harness::schedule::runner::check_record(&outcome);
        let output = if outcome.passed() {
            record
                .verify
                .as_ref()
                .filter(|verify| !verify.passed)
                .map(|verify| verify.output.clone())
                .unwrap_or_default()
        } else {
            detail.output.clone()
        };
        let verify = rimz::store::run::RunVerify {
            cmd: cmd.to_owned(),
            attempts: verify_attempt,
            passed: outcome.passed(),
            code: detail.code,
            timed_out: detail.timed_out,
            output,
        };
        let status = supervised::output::verify_status_label(&verify);
        let reprompt = rimz::harness::prompt_compose::verify_reprompt(cmd, &status, &verify.output);
        record = match rimz::harness::run::settle_verify(
            prepared.store.paths(),
            &record.run_id,
            verify,
            outcome.terminated() || waiter.waiter.cancellation().is_requested(),
            max_attempts,
        )? {
            VerifyStep::Settled(settled) => {
                record = settled;
                break;
            }
            VerifyStep::Reprompt(reopened) => reopened,
        };
        writeln!(
            render::err(),
            "rimz: verify `{cmd}` exited {status}; re-prompting (attempt {} of {max_attempts})",
            verify_attempt + 1,
        )?;
        if let Err(err) = supervised::verify::deliver_reprompt(
            &prepared.workspace,
            &prepared.store,
            &record,
            reprompt,
        ) {
            if let Some(failed) = rimz::harness::run::fail_if_nonterminal(
                prepared.store.paths(),
                &record.run_id,
                &render::error_line(&err),
            )? {
                record = failed;
            }
            verify_error = Some(err);
            break;
        }
        record = waiter.await_terminal(prepared, room, request)?;
        verify_attempt += 1;
    }
    Ok((record, verify_error, waiter))
}

fn close_attempt_pane(
    prepared: &AttemptContext,
    room: &rimz::room::RoomContext,
    record: &RunRecord,
) {
    let closed = if record.status == RunStatus::Canceled {
        supervised::pane::close_stopped_run_pane_after_grace(
            room.backend(),
            &prepared.store,
            &prepared.workspace.session_name,
            record,
            supervised::pane::STOP_BACKSTOP_GRACE,
        )
        .map_err(|open| open.to_string())
    } else {
        supervised::pane::close_run_pane(
            room.backend(),
            &prepared.store,
            &prepared.workspace.session_name,
            record,
        )
        .map_err(|err| err.to_string())
    };
    if let Err(error) = closed {
        tracing::debug!(run_id = %record.run_id, %error, "run cleanup left the pane open");
    }
}

fn handoff_termination(
    termination: &mut Option<CheckSignalGuard>,
    slot: &Mutex<Option<CheckSignalGuard>>,
) {
    if let Some(armed) = slot
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
    {
        *termination = Some(armed);
    }
}

pub(in crate::cli) fn run_supervised(
    request: SupervisedRunRequest,
    presentation: SupervisedPresentation,
    globals: &GlobalFlags,
    termination: &mut Option<CheckSignalGuard>,
) -> Result<Option<SupervisedRunOutcome>> {
    let mut interaction = crate::cli::launch_interaction::CliInteraction;
    let termination_slot = Arc::new(Mutex::new(None));
    let completion = if request.background {
        rimz::launcher::RunCompletion::Detached
    } else {
        rimz::launcher::RunCompletion::Watched {
            cancellation: supervised::run_interrupt_flag(),
            install_interrupts: |cancellation| {
                cancellation.reset();
                supervised::install_run_interrupt_handlers(cancellation)
            },
            termination: request.loop_task.as_ref().map(|_| termination_slot.clone()),
        }
    };
    let preparation = rimz::launcher::prepare_supervised(
        &request,
        rimz::launcher::SupervisedContext {
            root: globals.root.clone(),
            worktree_fresh_command: presentation.worktree_fresh_command.clone(),
            machine: crate::cli::machine_config(),
            mux: globals.mux,
            recovery: if std::io::stdin().is_terminal() {
                rimz::room::AttendedRecovery::Reset
            } else {
                rimz::room::AttendedRecovery::RequireExplicitReset
            },
            completion,
        },
        &mut interaction,
    )?;
    let mut next = match preparation {
        rimz::launcher::Preparation::Prepared(prepared) => *prepared,
        rimz::launcher::Preparation::Handled(_) => return Ok(None),
    };
    let retries = request.retries;
    let mut base_prompt = None;
    let mut attempt = 0;
    loop {
        let opened = rimz::launcher::execute(next, &mut interaction);
        handoff_termination(termination, &termination_slot);
        let opened = match opened? {
            rimz::launcher::LaunchOutcome::Opened(opened) => opened,
            rimz::launcher::LaunchOutcome::BudgetExceeded { reason } => {
                return Ok(Some(SupervisedRunOutcome::BudgetExceeded { reason }));
            }
            rimz::launcher::LaunchOutcome::Declined | rimz::launcher::LaunchOutcome::Existing => {
                return Ok(None);
            }
        };
        let mut run = opened
            .run
            .context("supervised launch returned no leader run")?;
        if request.background {
            return Ok(Some(SupervisedRunOutcome::Background {
                agent_name: run
                    .record
                    .agent_name
                    .clone()
                    .context("run has no agent name")?,
                run_id: run.record.run_id,
                response_path: run.response_path,
            }));
        }
        let prepared = AttemptContext {
            workspace: run.workspace.clone(),
            store: run.store.clone(),
            adapter: rimz::agents::find_definition(run.record.kind.as_str())
                .context("supervised run has no agent definition")?,
            cwd: opened.cwd,
            owns_checkout_lifecycle: run.owns_checkout_lifecycle,
            output_format: presentation.output_format,
            stream_text: presentation.stream_text,
        };
        let room = run.room.clone();
        let base_prompt = base_prompt.get_or_insert_with(|| run.record.prompt.clone());
        let mut waiter = PresentationWaiter {
            waiter: run
                .waiter
                .take()
                .context("blocking run did not bind its completion waiter")?,
            stream_cursor: (presentation.output_format == OutputFormat::StreamJson
                || presentation.stream_text)
                .then(|| TranscriptCursor::new(true)),
        };
        let record = waiter.await_terminal(&prepared, &room, &request)?;
        let blocking = BlockingAttempt { record, waiter };
        let (record, verify_error, waiter) = verify_phase(&prepared, &room, &request, blocking)?;
        join_presented_attempt(&prepared.store, &prepared.workspace.session_name, &record);
        if !request.keep {
            close_attempt_pane(&prepared, &room, &record);
        }
        drop(waiter);
        if let Some(err) = verify_error {
            return Err(err);
        }
        if !record.status.is_retryable() || attempt == retries {
            if retries > 0
                && prepared.owns_checkout_lifecycle
                && let Err(err) =
                    crate::cli::worktree::cleanup_worktree(&prepared.cwd, globals, false)
            {
                let _ = writeln!(
                    render::err(),
                    "rimz: worktree cleanup did not complete: {err}"
                );
            }
            return Ok(Some(SupervisedRunOutcome::Record(Box::new(record))));
        }
        let mut stderr = render::err();
        supervised::output::print_run_forensics(&record, &mut stderr)?;
        writeln!(
            stderr,
            "rimz: retrying (attempt {} of {})",
            u64::from(attempt) + 2,
            u64::from(retries) + 1,
        )?;
        let prompt = rimz::harness::prompt_compose::retry_prompt(
            base_prompt,
            record.failure_tail.as_deref(),
        );
        next = run.retry(prompt, record.run_id.clone());
        attempt += 1;
    }
}

/// A blocking attempt's result reaches its caller inline, so it must not also
/// wake a launching agent with a fleet report. Stamp it before the pane closes,
/// which is what lets the in-pane wrapper evaluate the report; a caller killed
/// before this point leaves the run to the report.
pub(super) fn join_presented_attempt(store: &rimz::Store, session_name: &str, record: &RunRecord) {
    if !record.status.is_terminal() {
        return;
    }
    if let Err(err) = rimz::harness::run::report::join_and_settle_digest(
        store,
        session_name,
        &record.run_id,
        Some(record.follow_ups + 1),
        "joined inline",
    ) {
        tracing::warn!(
            run_id = %record.run_id,
            error = %err,
            "could not mark the blocking run joined",
        );
    }
}

fn record_failure_tail_before_cleanup(
    backend: &dyn rimz::mux::MuxBackend,
    store: &rimz::Store,
    session_name: &str,
    record: RunRecord,
) -> RunRecord {
    if record.status == RunStatus::Completed || record.failure_tail.is_some() {
        return record;
    }
    let Some(pane) = supervised::pane::resolve_run_pane(store, session_name, &record) else {
        return record;
    };
    let Some(tail) =
        supervised::pane::capture_failure_tail(backend, &pane.pane_id, &pane.session_name)
    else {
        return record;
    };
    match rimz::harness::run::record_failure_tail(store.paths(), &record.run_id, &tail) {
        Ok(record) => record,
        Err(err) => {
            tracing::debug!(
                run_id = %record.run_id,
                pane = %pane.pane_id,
                error = %err,
                "could not record supervised run failure pane tail",
            );
            record
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unarmed_attempt_keeps_the_callers_termination_guard() {
        let mut termination = Some(
            rimz::harness::schedule::runner::cancel_run_on_sigterm(RunCancellation::new()).unwrap(),
        );
        let slot = Mutex::new(None);

        handoff_termination(&mut termination, &slot);

        assert!(
            termination.is_some(),
            "an early launch failure must retain the caller's guard through terminal recording"
        );
    }
}
