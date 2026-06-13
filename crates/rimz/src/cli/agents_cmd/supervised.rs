//! Supervised one-shot run support used by `rimz agents -p` and wait/stop.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::cli::GlobalFlags;
use rimz::agents::{AgentAdapter, hook_trust_fix};
use rimz::bridge::{self, ExpectedRunFrame, RunWakeOutcome};
use rimz::mux::PaneCmd;
use rimz::run::RunRecord;
use rimz::workspace::WorkspaceResolver;

pub(super) mod output;
pub(super) mod pane;
pub(super) mod stream;

#[cfg(test)]
use output::RunStreamEvent;
#[cfg(test)]
use pane::{ensure_sendable, latest_resolved_run_pane, resolve_run_pane_in_snapshot};
#[cfg(test)]
use stream::{TranscriptCursor, stream_attached_run, stream_blocking_run};

pub(super) fn resolve_run_workspace(globals: &GlobalFlags) -> Result<rimz::ResolvedWorkspace> {
    WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving current workspace")
}

pub(super) fn preflight_agent(adapter: &dyn AgentAdapter) -> Result<()> {
    let kind = adapter.descriptor().kind;
    if !adapter.hooks_installed() {
        bail!(
            "`rimz agents -p` requires {kind} hooks so the supervised turn can report completion; run `rimz hooks install {kind}`"
        );
    }
    let untrusted = adapter.untrusted_installed_hooks();
    if !untrusted.is_empty() {
        bail!(
            "{kind} hooks are installed but not trusted ({}); {}",
            untrusted.join(", "),
            hook_trust_fix(kind)
        );
    }
    Ok(())
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
    let resolves = rimz::launch::program_resolves_after_shell_rc(launch_env, program)
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
    pub(super) launch_id: Option<&'a rimz::ids::AgentSessionId>,
    pub(super) cwd: &'a Path,
    pub(super) prompt: &'a str,
    pub(super) cleanup_worktree: bool,
    pub(super) permission_args: &'a [String],
    pub(super) self_cleanup_on_completion: bool,
}

pub(super) fn run_pane_cmd(args: RunPaneCmdArgs<'_>) -> Result<PaneCmd> {
    let rimz_bin = std::env::current_exe().context("locating the rimz executable")?;
    let mut argv = vec![
        rimz_bin.to_string_lossy().into_owned(),
        "agents".to_owned(),
        "exec".to_owned(),
        args.adapter.descriptor().kind.to_owned(),
        "--run-id".to_owned(),
        args.run_id.to_string(),
    ];
    if let Some(agent_name) = args.agent_name {
        argv.extend(["--agent-name".to_owned(), agent_name.to_owned()]);
    }
    if let Some(launch_id) = args.launch_id {
        argv.extend(["--launch-id".to_owned(), launch_id.as_str().to_owned()]);
    }
    if args.self_cleanup_on_completion {
        argv.extend([
            "--exit-on-run-completion".to_owned(),
            "--close-pane-on-exit".to_owned(),
        ]);
    }
    if args.cleanup_worktree {
        argv.extend([
            "--worktree-path".to_owned(),
            args.cwd.to_string_lossy().into_owned(),
        ]);
    }
    argv.extend(["--prompt".to_owned(), args.prompt.to_owned()]);
    if !args.permission_args.is_empty() {
        argv.push("--".to_owned());
        argv.extend(args.permission_args.iter().cloned());
    }
    Ok(PaneCmd { argv })
}

pub(super) fn wait_for_run(
    sock: std::os::unix::net::UnixDatagram,
    expected: ExpectedRunFrame,
    timeout: Option<Duration>,
) -> Result<RunWakeOutcome> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .context("creating run wait runtime")?;
    runtime
        .block_on(bridge::wait_for_run_completion_owning(
            sock, expected, timeout,
        ))
        .context("waiting for run completion")
}

pub(super) fn terminal_record_after_wait(
    paths: &rimz::StatePaths,
    run_id: &rimz::RunId,
    outcome: RunWakeOutcome,
) -> Result<RunRecord> {
    match outcome {
        RunWakeOutcome::Completed(_status) => Ok(rimz::run::load(paths, run_id)?),
        RunWakeOutcome::Neutral => {
            let current = rimz::run::load(paths, run_id)?;
            if current.status.is_terminal() {
                Ok(current)
            } else {
                Ok(rimz::run::timeout(paths, run_id)?)
            }
        }
    }
}

pub(super) fn parse_timeout(raw: &str) -> std::result::Result<Duration, String> {
    crate::cli::parse::parse_duration_units(raw, &[("s", 1), ("m", 60), ("h", 3600), ("d", 86_400)])
}

#[cfg(test)]
mod tests;
