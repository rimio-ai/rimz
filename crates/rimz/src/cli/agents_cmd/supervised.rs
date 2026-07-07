//! Supervised one-shot run support used by `rimz agents -p` and wait/stop.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::cli::GlobalFlags;
use rimz::agents::{AgentAdapter, hook_trust_fix};
use rimz::harness::run::RunRecord;
use rimz::harness::run_wake::{self, ExpectedRunFrame, RunWakeOutcome};
use rimz::mux::PaneCmd;
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
    pub(super) agent_role: Option<&'a str>,
    pub(super) agent_channel: Option<&'a str>,
    pub(super) agent_model: Option<&'a str>,
    pub(super) agent_effort: Option<&'a str>,
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
                role: args.agent_role,
                channel: args.agent_channel,
                model: args.agent_model,
                effort: args.agent_effort,
                ..rimz::harness::launch::ExecIdentity::default()
            },
        },
    );
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
        .block_on(run_wake::wait_for_run_completion_owning(
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
        RunWakeOutcome::Completed(_status) => Ok(rimz::harness::run::load(paths, run_id)?),
        RunWakeOutcome::Neutral => {
            let current = rimz::harness::run::load(paths, run_id)?;
            if current.status.is_terminal() {
                Ok(current)
            } else {
                Ok(rimz::harness::run::timeout(paths, run_id)?)
            }
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

pub(super) fn combine_text_prompt(positional: Option<&str>, piped: Option<&str>) -> Result<String> {
    let positional = positional
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty());
    let piped = piped.map(str::trim).filter(|prompt| !prompt.is_empty());
    match (positional, piped) {
        (Some(positional), Some(piped)) => Ok(format!("{positional}\n\n{piped}")),
        (Some(positional), None) => Ok(positional.to_owned()),
        (None, Some(piped)) => Ok(piped.to_owned()),
        (None, None) => bail!(
            "expected a prompt for `rimz agents <spec> -p` (positional PROMPT or piped stdin)"
        ),
    }
}

pub(super) fn read_piped_text_prompt() -> Result<Option<String>> {
    use std::io::{IsTerminal as _, Read as _};

    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Ok(None);
    }
    let mut buf = String::new();
    stdin
        .lock()
        .read_to_string(&mut buf)
        .context("reading stdin")?;
    Ok(Some(buf))
}

#[cfg(test)]
mod tests;
