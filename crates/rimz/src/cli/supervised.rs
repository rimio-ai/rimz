//! Command-neutral supervised-run effects and presentation.

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::ValueEnum;

use crate::cli::GlobalFlags;
use rimz::agents::{AgentDefinition, HookPreflightErr, TurnLifecycleNeed, preflight_hooks};
use rimz::harness::run::RunCancellation;
use rimz::mux::PaneCmd;
use rimz::store::run::RunRecord;
use rimz::utils::time::{DurationUnit, parse_duration_units};
use rimz::workspace::WorkspaceResolver;

pub(super) mod output;
pub(super) mod pane;
pub(super) mod run;
pub(super) mod stream;
mod verify;

/// Output projection for a supervised `--print` run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub(in crate::cli) enum OutputFormat {
    /// The final assistant message as plain text.
    #[default]
    Text,
    /// The full run record as pretty JSON.
    Json,
    /// Newline-delimited JSON run events (NDJSON).
    StreamJson,
}

pub(in crate::cli) struct SupervisedPresentation {
    pub(in crate::cli) output_format: OutputFormat,
    pub(in crate::cli) stream_text: bool,
}

impl SupervisedPresentation {
    pub(in crate::cli) fn text(stream_text: bool) -> Self {
        Self {
            output_format: OutputFormat::Text,
            stream_text,
        }
    }
}

static RUN_INTERRUPT_SIGNAL_RECEIVED: OnceLock<RunCancellation> = OnceLock::new();
static RUN_INTERRUPT_HANDLERS_INSTALLED: OnceLock<()> = OnceLock::new();
const SUBAGENT_STOPPED_EVENT: &str = "rimz.subagent-stopped";

#[cfg(test)]
use output::RunStreamEvent;
#[cfg(test)]
use pane::{latest_resolved_run_pane, resolve_run_pane_in_snapshot};
#[cfg(test)]
use stream::{stream_attached_run, stream_blocking_run};

fn resolve_run_workspace(globals: &GlobalFlags) -> Result<rimz::ResolvedWorkspace> {
    WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving current workspace")
}

fn anchor_subagent_workspace(
    workspace: rimz::ResolvedWorkspace,
    request: &rimz::harness::run::SupervisedRunRequest,
    caller: Option<&rimz::agents::AgentState>,
    globals: &GlobalFlags,
) -> Result<rimz::ResolvedWorkspace> {
    let Some(parent) = caller.filter(|_| request.subagent) else {
        return Ok(workspace);
    };
    let Some(path) = parent.worktree_path.as_deref() else {
        tracing::debug!("subagent parent has no recorded checkout; keeping invoking cwd");
        return Ok(workspace);
    };
    if !Path::new(path).is_dir() {
        bail!(
            "the parent's checkout `{path}` no longer exists; restart the parent from an existing checkout, or launch with `rimz agents` from the directory the child should work in"
        );
    }
    let mut anchored = WorkspaceResolver::resolve_participant(path, globals.root.clone())
        .with_context(|| format!("resolving the parent's checkout `{path}`"))?;
    anchored.worktree_root = Path::new(path)
        .canonicalize()
        .with_context(|| format!("resolving the parent's launch directory `{path}`"))?;
    Ok(anchored)
}

fn preflight_agent(
    adapter: &AgentDefinition,
    launch: &rimz::worktree::LaunchCheckout,
    login: &rimz::agents::ProviderLogin,
) -> Result<()> {
    let definition = adapter.spec();
    let kind = definition.kind;
    let login_env = login.env(&rimz::agents::ambient_env());
    match preflight_hooks(adapter, &login_env, TurnLifecycleNeed::Wired) {
        Ok(()) => {}
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
    if let Err(error) = rimz::agents::preflight_launch_dir(
        adapter,
        &launch.cwd,
        launch.repo_root.as_deref(),
        &login_env,
    ) {
        bail!(
            "`rimz agents -p` cannot start {kind} in `{}`: {kind} has not recorded a trust decision for that directory and would stop at its trust prompt instead of taking the task; {}",
            launch.cwd.display(),
            error.fix
        );
    }
    Ok(())
}

fn preflight_program(
    adapter: &AgentDefinition,
    process: &rimz::harness::launch::CompiledAgentProcess,
) -> Result<()> {
    let program = &process.provider_program;
    let path = process
        .resolve_program_after_shell_rc()
        .with_context(|| format!("checking `{program}` after shell startup"))?;
    let Some(path) = path else {
        bail!("finding `{program}` on PATH after shell startup");
    };
    rimz::agents::version::check_launch_version_floor(adapter, &path)?;
    Ok(())
}

/// Why a supervised stop did not end with the run terminal and no nameable pane left.
#[derive(Debug, thiserror::Error)]
pub(crate) enum StopRunErr {
    /// The cancel failed; nothing was closed.
    #[error(transparent)]
    NotCanceled(anyhow::Error),
    /// The run is terminal and its pane was not confirmed closed.
    #[error("{0}; rerun the stop to close it")]
    PaneOpen(pane::PaneOpen),
    /// The run is terminal and its pane is gone, but its child's end was not recorded.
    #[error("run stopped, but recording its child's end failed: {0:#}; rerun the stop")]
    NotEnded(anyhow::Error),
}

/// Cancel a live supervised run, then reclaim its pane after the existing
/// backend grace. Terminal `--keep` records remain terminal and only lose the
/// pane. `Ok` means the run is terminal and no pane rimz can name for it
/// remains: the pane is gone, its session is gone, or none was ever recorded or
/// registered, so a pane nothing names can outlive an `Ok`.
/// For a subagent run, `Ok` also means its still-un-ended card is ended,
/// matched by the run's session id, else the newest same-kind namesake
/// registered at or before the run finished. A later namesake is never the
/// stop's.
pub(crate) fn stop_supervised_run(
    workspace: &rimz::ResolvedWorkspace,
    store: &rimz::Store,
    globals: &GlobalFlags,
    run: &RunRecord,
) -> std::result::Result<(), StopRunErr> {
    cancel_supervised_run(store, run).map_err(StopRunErr::NotCanceled)?;
    let backend = pane::backend_for_workspace_session(workspace, globals).map_err(|err| {
        StopRunErr::PaneOpen(pane::PaneOpen {
            pane: None,
            reason: format!("{err:#}"),
        })
    })?;
    pane::close_stopped_run_pane_after_grace(
        backend.as_ref(),
        store,
        &workspace.session_name,
        run,
        pane::STOP_BACKSTOP_GRACE,
    )
    .map_err(StopRunErr::PaneOpen)?;
    stamp_stopped_subagent_end(store, &workspace.session_name, run).map_err(StopRunErr::NotEnded)
}

pub(crate) fn cancel_supervised_run(store: &rimz::Store, run: &RunRecord) -> Result<()> {
    if !run.status.is_terminal() {
        rimz::harness::run::cancel_and_wake(store, &run.run_id)?;
    }
    Ok(())
}

fn stamp_stopped_subagent_end(
    store: &rimz::Store,
    session_name: &str,
    run: &RunRecord,
) -> Result<()> {
    if !run.subagent {
        return Ok(());
    }
    let audit = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .context("reading stopped subagent card")?;
    let Some(child) = audit
        .agents
        .iter()
        .filter(|agent| agent.ended_at.is_none() && run.matches_stopped_agent(agent))
        .max_by_key(|agent| {
            (
                run.agent_id.as_ref() == Some(&agent.agent_id),
                agent.registered_at,
            )
        })
    else {
        return Ok(());
    };
    let observation = rimz::agents::AgentLifecycleObservation::new(
        Some(child.agent_id.clone()),
        rimz::agents::LifecycleSignal::Ended,
    );
    let ended = rimz::EventEnvelope::agent_lifecycle(
        run.workspace_id.clone(),
        session_name,
        child.kind.as_str(),
        SUBAGENT_STOPPED_EVENT,
        &observation,
    );
    store
        .append_event(&ended)
        .context("recording stopped subagent end")
}

fn run_pane_cmd(
    runtime: &rimz::RuntimePaths,
    request: &rimz::harness::launch::ExecRequest,
) -> Result<PaneCmd> {
    let argv = rimz::harness::launch::exec_argv(&rimz::proc::rimz_exe(), runtime, request)?;
    Ok(PaneCmd {
        argv,
        name: Some(request.kind.to_string()),
    })
}

pub(in crate::cli) fn run_exit_policy(self_cleanup_on_completion: bool) -> (bool, bool) {
    (self_cleanup_on_completion, self_cleanup_on_completion)
}

pub(in crate::cli) fn note_isolation_clamp(
    profile: &str,
    source: &rimz::harness::plan::ClampSource,
) -> Result<()> {
    use std::io::Write as _;
    writeln!(
        std::io::stderr(),
        "rimz: {profile} runs sandboxed: {source}, and a subagent never runs looser than its parent"
    )?;
    Ok(())
}

fn install_run_interrupt_flag() -> Result<RunCancellation> {
    let flag = RUN_INTERRUPT_SIGNAL_RECEIVED
        .get_or_init(RunCancellation::new)
        .clone();
    flag.reset();
    install_run_interrupt_handlers(flag.clone())?;
    Ok(flag)
}

#[cfg(unix)]
fn install_run_interrupt_handlers(cancellation: RunCancellation) -> Result<()> {
    use signal_hook::consts::signal::SIGINT;

    if RUN_INTERRUPT_HANDLERS_INSTALLED.get().is_some() {
        return Ok(());
    }
    let flag = cancellation.signal_flag();
    signal_hook::flag::register_conditional_shutdown(SIGINT, 130, flag.clone())?;
    signal_hook::flag::register(SIGINT, flag)?;
    let _ = RUN_INTERRUPT_HANDLERS_INSTALLED.set(());
    Ok(())
}

#[cfg(not(unix))]
fn install_run_interrupt_handlers(_cancellation: RunCancellation) -> Result<()> {
    Ok(())
}

pub(super) fn parse_timeout(raw: &str) -> std::result::Result<Duration, String> {
    parse_duration_units(
        raw,
        &[
            DurationUnit::Second,
            DurationUnit::Minute,
            DurationUnit::Hour,
            DurationUnit::Day,
        ],
    )
    .map_err(|err| err.to_string())
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
