//! Process edge for the in-pane step every launch runs between its shell and its provider:
//! find the provider program on the pane's own PATH, hold it to the version floor, add the
//! arguments that depend on its version, prepare the model session, and exec it. On refusal
//! (including a failed exec), fail the explicitly named supervised run with the cleaned reason
//! before printing `rimz: <reason>` and exiting 78.

use rimz::agents::capabilities::{SESSION_STEP_REFUSED, SessionStepOutput, SessionStepRequest};
use rimz::ids::{AgentKind, AgentSessionId};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, clap::Args)]
pub(super) struct Args {
    kind: String,
    #[arg(long, allow_hyphen_values = true)]
    model: Option<String>,
    #[arg(long, allow_hyphen_values = true)]
    effort: Option<String>,
    #[arg(long)]
    session: Option<String>,
    /// The launch resumes or forks a session whose configured system prompt changed.
    #[arg(long)]
    changed_system_prompt: bool,
    /// Supervised run id this step belongs to.
    #[arg(long)]
    run_id: Option<rimz::ids::RunId>,
    #[arg(last = true, required = true)]
    provider_argv: Vec<String>,
}

pub(super) fn run(args: Args) -> anyhow::Result<()> {
    let prepare = || -> Result<(PathBuf, SessionStepOutput), String> {
        let adapter =
            rimz::agents::definition_by_kind(&args.kind).map_err(|err| err.to_string())?;
        let cwd = std::env::current_dir().map_err(|err| err.to_string())?;
        let program = &args.provider_argv[0];
        let path = which::which(program).map_err(|_| format!("cannot find `{program}` on PATH"))?;
        rimz::agents::version::check_launch_version_floor(adapter, &path)
            .map_err(|err| err.to_string())?;
        let mut provider_argv = args.provider_argv.clone();
        if args.changed_system_prompt {
            provider_argv.extend(adapter.changed_system_prompt_args(&path));
        }
        let Some(model) = &args.model else {
            let output = SessionStepOutput {
                argv: provider_argv,
                notices: Vec::new(),
            };
            return Ok((path, output));
        };
        let session = args.session.as_deref().map(AgentSessionId::from);
        let output = adapter
            .session_step(&SessionStepRequest {
                session: session.as_ref(),
                model,
                effort: args.effort.as_deref(),
                provider_argv: &provider_argv,
                cwd: &cwd,
                env: &rimz::agents::ambient_env(),
            })
            .ok_or_else(|| format!("agent `{}` does not support session preparation", args.kind))?
            .map_err(|refusal| refusal.to_string())?;
        Ok((path, output))
    };
    let (path, output) = match prepare() {
        Ok(prepared) => prepared,
        Err(reason) => return refuse(args.run_id.as_ref(), reason),
    };
    let Some((program, argv)) = output.argv.split_first() else {
        return refuse(
            args.run_id.as_ref(),
            "session preparation returned an empty command".into(),
        );
    };
    let notices: Vec<String> = output
        .notices
        .into_iter()
        .map(|notice| {
            notice
                .chars()
                .map(|ch| if ch.is_control() { ' ' } else { ch })
                .collect()
        })
        .collect();
    for notice in &notices {
        writeln!(crate::cli::render::err(), "rimz: {notice}")?;
    }
    if !notices.is_empty()
        && let Err(error) = record_notices(&args, notices)
    {
        tracing::debug!(%error, "could not record session-step launch notices");
    }
    // Exec the path the floor checked, under the argv[0] the launch wrote.
    let mut command = Command::new(&path);
    command.args(argv);
    #[cfg(unix)]
    let error = {
        use std::os::unix::process::CommandExt;
        command.arg0(program).exec()
    };
    #[cfg(not(unix))]
    let error = std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "session preparation requires Unix exec",
    );
    refuse(
        args.run_id.as_ref(),
        format!("could not exec provider `{program}`: {error}"),
    )
}

fn record_notices(args: &Args, notices: Vec<String>) -> anyhow::Result<()> {
    let launch_id = std::env::var(rimz::harness::launch::ENV_AGENT_ID)
        .ok()
        .filter(|id| !id.is_empty())
        .map(AgentSessionId::from);
    let Some(agent_id) = args
        .session
        .as_deref()
        .map(AgentSessionId::from)
        .or_else(|| launch_id.clone())
    else {
        return Ok(());
    };
    let workspace = rimz::WorkspaceResolver::resolve_participant(".", None)?;
    let store = crate::cli::require_existing_store(&workspace)?;
    let kind = AgentKind::new_unchecked(&args.kind);
    let mut warnings = store
        .snapshot()?
        .agents
        .iter()
        .find(|agent| agent.kind == kind && agent.agent_id == agent_id)
        .map(|agent| agent.launch_warnings.clone())
        .unwrap_or_default();
    warnings.extend(notices);
    store.record_launch_warnings(
        &kind,
        &agent_id,
        launch_id.as_ref(),
        &workspace.session_name,
        warnings,
    )?;
    Ok(())
}

fn fail_refused_run(run_id: &rimz::ids::RunId, reason: &str) -> anyhow::Result<()> {
    let workspace = rimz::WorkspaceResolver::resolve_participant(".", None)?;
    let store = crate::cli::require_existing_store(&workspace)?;
    rimz::harness::run::fail_step_refusal(
        store.paths(),
        store.runtime_paths(),
        run_id,
        rimz::mux::ambient_pane_id().as_ref(),
        reason,
    )?;
    Ok(())
}

fn refuse(run_id: Option<&rimz::ids::RunId>, reason: String) -> anyhow::Result<()> {
    let reason: String = reason
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    if let Some(run_id) = run_id
        && let Err(error) = fail_refused_run(run_id, &reason)
    {
        tracing::debug!(%run_id, %error, "could not record session-step refusal on run");
    }
    writeln!(crate::cli::render::err(), "rimz: {reason}")?;
    Err(
        anyhow::Error::new(crate::cli::render::AlreadyReported).context(
            crate::cli::usage::ExitCodeError {
                code: SESSION_STEP_REFUSED,
                message: reason,
            },
        ),
    )
}
