//! Process edge for an adapter's pre-exec session preparation.

use rimz::agents::capabilities::{SESSION_STEP_REFUSED, SessionStepRequest};
use rimz::ids::AgentSessionId;
use std::io::Write;
use std::process::Command;

#[derive(Debug, clap::Args)]
pub(super) struct Args {
    kind: String,
    #[arg(long, allow_hyphen_values = true)]
    model: String,
    #[arg(long)]
    session: Option<String>,
    #[arg(last = true, required = true)]
    provider_argv: Vec<String>,
}

pub(super) fn run(args: Args) -> anyhow::Result<()> {
    let prepare = || -> Result<Vec<String>, String> {
        let adapter =
            rimz::agents::definition_by_kind(&args.kind).map_err(|err| err.to_string())?;
        let cwd = std::env::current_dir().map_err(|err| err.to_string())?;
        let session = args.session.as_deref().map(AgentSessionId::from);
        adapter
            .session_step(&SessionStepRequest {
                session: session.as_ref(),
                model: &args.model,
                provider_argv: &args.provider_argv,
                cwd: &cwd,
                env: &rimz::agents::ambient_env(),
            })
            .ok_or_else(|| format!("agent `{}` does not support session preparation", args.kind))?
            .map_err(|refusal| refusal.to_string())
    };
    let argv = match prepare() {
        Ok(argv) => argv,
        Err(reason) => return refuse(reason),
    };
    let Some((program, argv)) = argv.split_first() else {
        return refuse("session preparation returned an empty command".into());
    };
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
