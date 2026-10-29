//! Process edge for an adapter's pre-exec session preparation.

use rimz::agents::capabilities::{SESSION_STEP_REFUSED, SessionStepOutput, SessionStepRequest};
use rimz::ids::{AgentKind, AgentSessionId};
use std::io::Write;
use std::process::Command;

#[derive(Debug, clap::Args)]
pub(super) struct Args {
    kind: String,
    #[arg(long, allow_hyphen_values = true)]
    model: String,
    #[arg(long, allow_hyphen_values = true)]
    effort: Option<String>,
    #[arg(long)]
    session: Option<String>,
    #[arg(last = true, required = true)]
    provider_argv: Vec<String>,
}

pub(super) fn run(args: Args) -> anyhow::Result<()> {
    let prepare = || -> Result<SessionStepOutput, String> {
        let adapter =
            rimz::agents::definition_by_kind(&args.kind).map_err(|err| err.to_string())?;
        let cwd = std::env::current_dir().map_err(|err| err.to_string())?;
        let session = args.session.as_deref().map(AgentSessionId::from);
        adapter
            .session_step(&SessionStepRequest {
                session: session.as_ref(),
                model: &args.model,
                effort: args.effort.as_deref(),
                provider_argv: &args.provider_argv,
                cwd: &cwd,
                env: &rimz::agents::ambient_env(),
            })
            .ok_or_else(|| format!("agent `{}` does not support session preparation", args.kind))?
            .map_err(|refusal| refusal.to_string())
    };
    let output = match prepare() {
        Ok(output) => output,
        Err(reason) => return refuse(reason),
    };
    let Some((program, argv)) = output.argv.split_first() else {
        return refuse("session preparation returned an empty command".into());
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
    let mut command = Command::new(program);
    command.args(argv);
    #[cfg(unix)]
    let error = {
        use std::os::unix::process::CommandExt;
        command.exec()
    };
    #[cfg(not(unix))]
    let error = std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "session preparation requires Unix exec",
    );
    refuse(format!("could not exec provider `{program}`: {error}"))
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

fn refuse(reason: String) -> anyhow::Result<()> {
    let reason: String = reason
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
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
