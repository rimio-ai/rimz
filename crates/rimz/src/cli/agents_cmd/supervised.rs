//! Supervised one-shot run support used by `rimz agents -p` and wait/stop.

use std::path::Path;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::cli::GlobalFlags;
use rimz::agents::{AgentAdapter, HookPreflightErr, TurnLifecycleNeed, preflight_hooks};
use rimz::harness::run::RunRecord;
use rimz::harness::run_wake::{self, ExpectedRunFrame, RunWakeOutcome};
use rimz::mux::PaneCmd;
use rimz::workspace::WorkspaceResolver;

pub(super) mod output;
pub(super) mod pane;
pub(super) mod run;
pub(super) mod stream;
pub(super) mod verify;

const RUN_WAIT_INTERRUPT_POLL: Duration = Duration::from_millis(250);
static RUN_INTERRUPT_SIGNAL_RECEIVED: OnceLock<Arc<AtomicBool>> = OnceLock::new();
static RUN_INTERRUPT_HANDLERS_INSTALLED: OnceLock<()> = OnceLock::new();

#[cfg(test)]
use output::RunStreamEvent;
#[cfg(test)]
use pane::{ensure_sendable, latest_resolved_run_pane, resolve_run_pane_in_snapshot};
#[cfg(test)]
use stream::{stream_attached_run, stream_blocking_run};

pub(super) fn resolve_run_workspace(globals: &GlobalFlags) -> Result<rimz::ResolvedWorkspace> {
    WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving current workspace")
}

pub(super) fn preflight_agent(adapter: &dyn AgentAdapter) -> Result<()> {
    let descriptor = adapter.descriptor();
    let kind = descriptor.kind;
    match preflight_hooks(adapter, TurnLifecycleNeed::Wired) {
        Ok(()) => Ok(()),
        Err(HookPreflightErr::TurnLifecycleUnsupported { reason }) => bail!(
            "`rimz agents -p` cannot supervise {kind}: a verified executable turn-lifecycle signal is required; {}",
            reason
        ),
        Err(HookPreflightErr::HooksMissing) => bail!(
            "`rimz agents -p` requires {kind} hooks so the supervised turn can report completion; run `rimz hooks install {kind}`"
        ),
        Err(HookPreflightErr::HooksUntrusted { hooks, fix }) => bail!(
            "{kind} hooks are installed but not trusted ({}); {}",
            hooks,
            fix
        ),
    }
}

pub(super) fn preflight_program(
    adapter: &dyn AgentAdapter,
    permission_args: &[String],
    prompt: &str,
    launch_env: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    let argv = adapter
        .launch_command(permission_args, Some(prompt))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "agent `{}` has no launch command",
                adapter.descriptor().kind
            )
        })?;
    let Some(program) = argv.first() else {
        bail!(
            "agent `{}` produced an empty launch command",
            adapter.descriptor().kind
        );
    };
    let resolves = rimz::harness::launch::program_resolves_after_shell_rc(launch_env, program)
        .with_context(|| format!("checking `{program}` after shell startup"))?;
    if !resolves {
        bail!("finding `{program}` on PATH after shell startup");
    }
    Ok(())
}

pub(super) struct RunPaneCmdArgs<'a> {
    pub(super) adapter: &'a dyn AgentAdapter,
    pub(super) run_id: &'a rimz::RunId,
    pub(super) agent_name: Option<&'a str>,
    pub(super) agent_name_explicit: bool,
    pub(super) agent_profile: Option<&'a str>,
    pub(super) agent_mode: Option<rimz::harness::run::PermissionMode>,
    pub(super) agent_role: Option<&'a str>,
    pub(super) agent_channel: Option<&'a str>,
    pub(super) agent_model: Option<&'a str>,
    pub(super) agent_effort: Option<&'a str>,
    pub(super) agent_budget: Option<&'a str>,
    pub(super) launch_id: Option<&'a rimz::ids::AgentSessionId>,
    pub(super) cwd: &'a Path,
    pub(super) prompt: &'a str,
    pub(super) cleanup_worktree: bool,
    pub(super) permission_args: &'a [String],
    pub(super) self_cleanup_on_completion: bool,
}

pub(super) fn run_pane_cmd(args: RunPaneCmdArgs<'_>) -> Result<PaneCmd> {
    let rimz_bin = rimz::proc::rimz_exe();
    let argv = rimz::harness::launch::exec_argv(
        &rimz_bin,
        &rimz::harness::launch::ExecInvocation {
            kind: args.adapter.descriptor().kind,
            action: rimz::harness::launch::ExecAction::Launch {
                prompt: Some(args.prompt),
                extra_args: args.permission_args,
            },
            run_id: Some(args.run_id.as_str()),
            worktree_path: args.cleanup_worktree.then_some(args.cwd),
            close_pane_on_exit: args.self_cleanup_on_completion,
            exit_on_run_completion: args.self_cleanup_on_completion,
            identity: rimz::harness::launch::ExecIdentity {
                name: args.agent_name,
                name_explicit: args.agent_name_explicit,
                launch_id: args.launch_id.map(rimz::ids::AgentSessionId::as_str),
                profile: args.agent_profile,
                mode: args.agent_mode,
                role: args.agent_role,
                channel: args.agent_channel,
                model: args.agent_model,
                effort: args.agent_effort,
                budget: args.agent_budget,
                ..rimz::harness::launch::ExecIdentity::default()
            },
        },
    );
    Ok(PaneCmd { argv })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RunWaitOutcome {
    Completed,
    TimedOut,
    Interrupted,
}

pub(super) fn install_run_interrupt_flag() -> Result<Arc<AtomicBool>> {
    let flag = RUN_INTERRUPT_SIGNAL_RECEIVED
        .get_or_init(|| Arc::new(AtomicBool::new(false)))
        .clone();
    flag.store(false, Ordering::SeqCst);
    install_run_interrupt_handlers(flag.clone())?;
    Ok(flag)
}

#[cfg(unix)]
fn install_run_interrupt_handlers(flag: Arc<AtomicBool>) -> Result<()> {
    use signal_hook::consts::signal::SIGINT;

    if RUN_INTERRUPT_HANDLERS_INSTALLED.get().is_some() {
        return Ok(());
    }
    signal_hook::flag::register_conditional_shutdown(SIGINT, 130, flag.clone())?;
    signal_hook::flag::register(SIGINT, flag)?;
    let _ = RUN_INTERRUPT_HANDLERS_INSTALLED.set(());
    Ok(())
}

#[cfg(not(unix))]
fn install_run_interrupt_handlers(_flag: Arc<AtomicBool>) -> Result<()> {
    Ok(())
}

pub(super) fn wait_for_run(
    sock: std::os::unix::net::UnixDatagram,
    expected: ExpectedRunFrame,
    timeout: Option<Duration>,
    interrupt: &AtomicBool,
) -> Result<RunWaitOutcome> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .context("creating run wait runtime")?;
    let outcome: Result<RunWaitOutcome> = runtime.block_on(async {
        let sock = run_wake::adopt(sock).context("adopting run socket")?;
        let deadline = timeout.map(|duration| Instant::now() + duration);
        loop {
            if interrupt.load(Ordering::SeqCst) {
                return Ok(RunWaitOutcome::Interrupted);
            }
            let Some(wait) = next_run_wait(deadline) else {
                return Ok(RunWaitOutcome::TimedOut);
            };
            match run_wake::wait_for_run_completion(&sock, &expected, Some(wait)).await? {
                RunWakeOutcome::Completed(_status) => return Ok(RunWaitOutcome::Completed),
                RunWakeOutcome::Neutral => {
                    if interrupt.load(Ordering::SeqCst) {
                        return Ok(RunWaitOutcome::Interrupted);
                    }
                    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                        return Ok(RunWaitOutcome::TimedOut);
                    }
                }
            }
        }
    });
    outcome.context("waiting for run completion")
}

fn next_run_wait(deadline: Option<Instant>) -> Option<Duration> {
    let Some(deadline) = deadline else {
        return Some(RUN_WAIT_INTERRUPT_POLL);
    };
    let now = Instant::now();
    if now >= deadline {
        None
    } else {
        Some((deadline - now).min(RUN_WAIT_INTERRUPT_POLL))
    }
}

pub(super) fn terminal_record_after_wait(
    paths: &rimz::StatePaths,
    run_id: &rimz::RunId,
    outcome: RunWaitOutcome,
) -> Result<RunRecord> {
    match outcome {
        RunWaitOutcome::Completed => Ok(rimz::harness::run::load(paths, run_id)?),
        RunWaitOutcome::TimedOut => {
            let current = rimz::harness::run::load(paths, run_id)?;
            if current.status.is_terminal() {
                Ok(current)
            } else {
                Ok(rimz::harness::run::timeout(paths, run_id)?)
            }
        }
        RunWaitOutcome::Interrupted => {
            let (record, _wrote) = rimz::harness::run::cancel(paths, run_id)?;
            Ok(record)
        }
    }
}

pub(super) fn parse_timeout(raw: &str) -> std::result::Result<Duration, String> {
    crate::cli::parse::parse_duration_units(raw, &[("s", 1), ("m", 60), ("h", 3600), ("d", 86_400)])
}

/// Extract the prompt from stream-json user messages on stdin. Each non-empty
/// line is one JSON object; `{"type":"user"}` envelopes contribute their
/// `message.content` text (a bare string, or the `text` of each text block),
/// joined with newlines. This is the standard headless stream-json input
/// schema, so the parser is provider-agnostic.
pub(super) fn read_stream_json_prompt<R: std::io::BufRead>(reader: R) -> Result<String> {
    let mut parts: Vec<String> = Vec::new();
    for line in reader.lines() {
        let line = line.context("reading stdin")?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(trimmed)
            .with_context(|| format!("parsing stream-json line `{trimmed}`"))?;
        if value.get("type").and_then(serde_json::Value::as_str) != Some("user") {
            continue;
        }
        match value
            .get("message")
            .and_then(|message| message.get("content"))
        {
            Some(serde_json::Value::String(text)) => parts.push(text.clone()),
            Some(serde_json::Value::Array(blocks)) => {
                for block in blocks {
                    if block.get("type").and_then(serde_json::Value::as_str) == Some("text")
                        && let Some(text) = block.get("text").and_then(serde_json::Value::as_str)
                    {
                        parts.push(text.to_owned());
                    }
                }
            }
            _ => {}
        }
    }
    Ok(parts.join("\n"))
}

#[cfg(test)]
mod tests;
