//! Command-neutral supervised-run effects and presentation.

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::ValueEnum;

use crate::cli::GlobalFlags;
use rimz::agents::{AgentAdapter, HookPreflightErr, TurnLifecycleNeed, preflight_hooks};
use rimz::harness::run::{RunCancellation, RunRecord};
use rimz::mux::PaneCmd;
use rimz::workspace::WorkspaceResolver;

pub(super) mod output;
pub(super) mod pane;
pub(super) mod run;
pub(super) mod stream;
pub(super) mod verify;

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
    process: &rimz::harness::launch::CompiledAgentProcess,
) -> Result<()> {
    let program = &process.provider_program;
    let resolves = rimz::harness::launch::program_resolves_after_shell_rc(&process.env, program)
        .with_context(|| format!("checking `{program}` after shell startup"))?;
    if !resolves {
        bail!("finding `{program}` on PATH after shell startup");
    }
    Ok(())
}

/// Cancel a live supervised run, then reclaim its pane after the existing
/// backend grace. Terminal `--keep` records remain terminal and only lose the
/// pane.
pub(crate) fn stop_supervised_run(
    workspace: &rimz::ResolvedWorkspace,
    store: &rimz::Store,
    globals: &GlobalFlags,
    run: &RunRecord,
) -> Result<()> {
    if !run.status.is_terminal() {
        rimz::harness::run::cancel_and_wake(store, &run.run_id)?;
    }
    if let Ok(backend) = pane::backend_for_workspace_session(workspace, globals) {
        pane::close_stopped_run_pane_after_grace(
            backend.as_ref(),
            store,
            &workspace.session_name,
            run,
            pane::STOP_BACKSTOP_GRACE,
        );
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

pub(super) fn install_run_interrupt_flag() -> Result<RunCancellation> {
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
