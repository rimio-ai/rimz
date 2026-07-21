//! `rimz web` — browser access through the shared ttyd daemon.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use super::{GlobalFlags, machine_config, open_browser_best_effort};
use crate::cli::room;
use rimz::ids::MuxName;
use rimz::mux::CommandSpec;
use rimz::room::session::{LiveSessions, workspace_record_for_session};
use rimz::web::{CredentialCommand, CredentialOutcome, WebCredential};

#[derive(Debug, Args)]
pub struct WebArgs {
    #[command(subcommand)]
    command: Option<WebSubcmd>,
}

#[derive(Debug, Subcommand)]
enum WebSubcmd {
    /// Open the current workspace's web URL.
    Open(WebOpenArgs),
    /// Print the current workspace's web URL without starting the daemon.
    Url(WebUrlArgs),
    /// Report the shared ttyd daemon's status.
    Status(WebStatusArgs),
    /// Start the shared ttyd daemon.
    Start,
    /// Stop the shared ttyd daemon.
    Stop,
    /// Manage the machine-wide browser credential.
    Token {
        #[command(subcommand)]
        command: WebTokenSubcmd,
    },
    /// Resolve and attach a ttyd client to a managed RimZ room.
    #[command(hide = true)]
    Exec {
        #[arg(value_name = "SESSION")]
        session: Option<String>,
    },
}

#[derive(Debug, Args)]
struct WebOpenArgs {
    /// Workspace path. Defaults to the current directory.
    #[arg(value_name = "PATH")]
    path: Option<PathBuf>,
    /// Existing RimZ session name to open instead of resolving a path.
    #[arg(long, conflicts_with = "path")]
    session: Option<String>,
    /// Print the URL without launching a browser.
    #[arg(long)]
    print: bool,
    /// Require the shared ttyd daemon to already be online.
    #[arg(long)]
    no_start: bool,
    /// Come up empty: skip recovering prior agents when the room is reborn.
    #[arg(long)]
    no_resume: bool,
    /// Prompt for resume even when stdin is not a terminal. Internal remote-web helper.
    #[arg(long, hide = true)]
    confirm_resume: bool,
    /// Emit the versioned `rimz.web.v2` payload.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct WebUrlArgs {
    /// Workspace path. Defaults to the current directory.
    #[arg(value_name = "PATH")]
    path: Option<PathBuf>,
    /// Existing RimZ session name to print instead of resolving a path.
    #[arg(long, conflicts_with = "path")]
    session: Option<String>,
    /// Emit the versioned `rimz.web.v2` payload.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct WebStatusArgs {
    /// Emit JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum WebTokenSubcmd {
    /// Rotate the browser credential.
    Create {
        /// Read-only credentials are unsupported by ttyd.
        #[arg(long)]
        read_only: bool,
    },
    /// List the machine credential and creation date.
    List,
    /// Revoke the machine credential by name.
    Revoke { name: String },
    /// Revoke the machine credential.
    RevokeAll,
    /// Provision the machine credential if needed.
    #[command(hide = true)]
    Ensure,
}

impl From<WebTokenSubcmd> for CredentialCommand {
    fn from(command: WebTokenSubcmd) -> Self {
        match command {
            WebTokenSubcmd::Create { read_only } => Self::Create { read_only },
            WebTokenSubcmd::List => Self::List,
            WebTokenSubcmd::Revoke { name } => Self::Revoke { name },
            WebTokenSubcmd::RevokeAll => Self::RevokeAll,
            WebTokenSubcmd::Ensure => Self::Ensure,
        }
    }
}

pub fn run(args: WebArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command.unwrap_or(WebSubcmd::Open(WebOpenArgs {
        path: None,
        session: None,
        print: false,
        no_start: false,
        no_resume: false,
        confirm_resume: false,
        json: false,
    })) {
        WebSubcmd::Open(args) => open(args, globals),
        WebSubcmd::Url(args) => url(args, globals),
        WebSubcmd::Status(args) => status(args),
        WebSubcmd::Start => start(),
        WebSubcmd::Stop => stop(),
        WebSubcmd::Token { command } => token(command),
        WebSubcmd::Exec { session } => exec(session.as_deref()),
    }
}

fn open(args: WebOpenArgs, globals: &GlobalFlags) -> Result<()> {
    let config = machine_config();
    if !config.web.enabled {
        bail!(
            "Browser access is disabled: set `[web] enabled = true` in the RimZ config on the machine serving this room (`rimz config path`) to allow browser sharing."
        );
    }
    let context = if let Some(session) = args.session {
        room::ensure_session_room_for_web(&session, globals, args.no_resume, args.confirm_resume)?
    } else {
        let path = args.path.unwrap_or_else(|| PathBuf::from("."));
        room::ensure_workspace_room_for_web(&path, globals, args.no_resume, args.confirm_resume)?
    };
    let session = context.session_name().to_owned();
    ensure_session_addressable_for_web(context.mux_name(), &session)?;
    let outcome = rimz::web::open_session(&session, &config, !args.no_start)?;
    crate::cli::render::web_warnings(&outcome.warnings);
    if args.json {
        return crate::cli::render::json(&outcome.payload);
    }
    print_url(&outcome.payload.url)?;
    write_web_credential(&outcome.payload.credential);
    if !args.print {
        open_browser_best_effort(&outcome.payload.url);
    }
    Ok(())
}

const WEB_ADDRESSABLE_TIMEOUT: Duration = Duration::from_secs(5);

fn web_addressable_timeout() -> Duration {
    let Some(value) =
        std::env::var_os("RIMZ_TEST_WEB_ADDRESSABLE_MS").filter(|value| !value.is_empty())
    else {
        return WEB_ADDRESSABLE_TIMEOUT;
    };
    value
        .to_str()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(WEB_ADDRESSABLE_TIMEOUT)
}

fn ensure_session_addressable_for_web(mux: MuxName, session: &str) -> Result<()> {
    let backend = rimz::mux::backend_for(mux);
    let deadline = Instant::now() + web_addressable_timeout();
    loop {
        match backend.list_sessions() {
            Ok(sessions) if sessions.iter().any(|name| name == session) => return Ok(()),
            Ok(_) => {
                if Instant::now() >= deadline {
                    bail!(
                        "{mux} session `{session}` is not addressable after web preparation. Run `rimz reset` from the workspace, then retry `rimz web open`."
                    );
                }
            }
            Err(err) => {
                let detail = err.to_string();
                if Instant::now() >= deadline {
                    bail!(
                        "{mux} session `{session}` is not addressable after web preparation: {detail}. Run `rimz reset` from the workspace, then retry `rimz web open`."
                    );
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn url(args: WebUrlArgs, globals: &GlobalFlags) -> Result<()> {
    let context = if let Some(session) = args.session {
        room::web_room_for_session(&session, globals)?
    } else {
        let path = args.path.unwrap_or_else(|| PathBuf::from("."));
        room::existing_web_room_for_path(&path, globals)?
    };
    let payload = rimz::web::inspect_session(context.session_name(), &machine_config())?;
    if args.json {
        crate::cli::render::json(&payload)
    } else {
        print_url(&payload.url)
    }
}

fn status(args: WebStatusArgs) -> Result<()> {
    let status = rimz::web::status(&machine_config())?;
    if args.json {
        return crate::cli::render::json(&status);
    }
    let mut stdout = std::io::stdout().lock();
    if let Some(pid) = status.pid {
        writeln!(
            stdout,
            "ttyd: online on 127.0.0.1:{} (pid {pid})",
            status.port
        )?;
    } else {
        writeln!(stdout, "ttyd: offline (configured port {})", status.port)?;
    }
    Ok(())
}

fn start() -> Result<()> {
    let outcome = rimz::web::ensure_daemon(&machine_config())?;
    crate::cli::render::web_warnings(&outcome.warnings);
    writeln!(
        std::io::stdout().lock(),
        "ttyd: online on 127.0.0.1:{} (pid {})",
        outcome.record.port,
        outcome.record.pid
    )?;
    Ok(())
}

fn stop() -> Result<()> {
    let stopped = rimz::web::stop_all()?;
    writeln!(
        std::io::stdout().lock(),
        "stopped {} ttyd daemon{}",
        stopped.stopped,
        if stopped.stopped == 1 { "" } else { "s" }
    )?;
    Ok(())
}

fn token(command: WebTokenSubcmd) -> Result<()> {
    render_credential_outcome(rimz::web::credential(command.into(), &machine_config())?)
}

fn render_credential_outcome(outcome: CredentialOutcome) -> Result<()> {
    match outcome {
        CredentialOutcome::Ensured(credential) => {
            writeln!(std::io::stdout().lock(), "{}", credential.secret)?;
        }
        CredentialOutcome::Rotated {
            credential,
            restarted_instances,
            warnings,
        } => {
            crate::cli::render::web_warnings(&warnings);
            write_web_credential(&credential);
            writeln!(
                std::io::stdout().lock(),
                "rotated ttyd credential and restarted {restarted_instances} daemon(s)"
            )?;
        }
        CredentialOutcome::Listed(credentials) => {
            let mut stdout = std::io::stdout().lock();
            for credential in credentials {
                writeln!(stdout, "{}: {}", credential.name, credential.created_at)?;
            }
        }
        CredentialOutcome::Revoked { stopped_instances } => {
            writeln!(
                std::io::stdout().lock(),
                "revoked ttyd credential and stopped {stopped_instances} daemon(s)"
            )?;
        }
    }
    Ok(())
}

fn exec(session: Option<&str>) -> Result<()> {
    let live = LiveSessions::probe();
    let target = session
        .filter(|session| !session.is_empty())
        .and_then(|session| {
            workspace_record_for_session(session)
                .ok()
                .flatten()
                .and_then(|_| live.mux_of(session).map(|mux| (session, mux)))
        });
    let Some((session, mux)) = target else {
        bail!(web_exec_session_error(session, &live)?);
    };
    exec_web_command(&web_exec_spec(session, mux))
}

fn web_exec_session_error(session: Option<&str>, live: &LiveSessions) -> Result<String> {
    let mut sessions = rimz::workspace::known_workspaces()
        .context("reading RimZ workspace records")?
        .into_iter()
        .filter_map(|workspace| {
            live.mux_of(&workspace.session_name)
                .map(|mux| format!("  {} ({mux})", workspace.session_name))
        })
        .collect::<Vec<_>>();
    sessions.sort();
    let requested = session.map_or_else(
        || "no session was provided".to_owned(),
        |session| format!("session `{session}` is not a live RimZ room"),
    );
    let listing = if sessions.is_empty() {
        "  (none)".to_owned()
    } else {
        sessions.join("\n")
    };
    Ok(format!("{requested}\n\nLive RimZ sessions:\n{listing}"))
}

fn web_exec_spec(session: &str, mux: MuxName) -> CommandSpec {
    match mux {
        MuxName::Tmux => rimz::mux::tmux::managed_cmd().args(["attach", "-t", session]),
        MuxName::Zellij => rimz::mux::zellij::attach_existing_command(session),
    }
}

#[cfg(unix)]
fn exec_web_command(spec: &CommandSpec) -> Result<()> {
    use std::os::unix::process::CommandExt as _;

    let err = spec.to_command().exec();
    Err(err).with_context(|| format!("execing `{}`", spec.display_line()))
}

#[cfg(not(unix))]
fn exec_web_command(spec: &CommandSpec) -> Result<()> {
    let status = spec
        .to_command()
        .status()
        .with_context(|| format!("running `{}`", spec.display_line()))?;
    if !status.success() {
        bail!("command `{}` exited with {status}", spec.display_line());
    }
    Ok(())
}

fn write_web_credential(credential: &WebCredential) {
    let _ = writeln!(
        std::io::stderr().lock(),
        "ttyd basic auth for this machine (browser will show a Basic-Auth prompt): user {}, password {}",
        credential.username,
        credential.secret
    );
}

fn print_url(url: &str) -> Result<()> {
    writeln!(std::io::stdout().lock(), "{url}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_exec_argv_targets_existing_session_on_each_mux() {
        let tmux = web_exec_spec("rimz-test", MuxName::Tmux);
        assert_eq!(
            &tmux.args[tmux.args.len() - 3..],
            ["attach", "-t", "rimz-test"]
        );

        let zellij = web_exec_spec("rimz-test", MuxName::Zellij);
        assert_eq!(zellij.args, ["attach", "rimz-test"]);
    }
}
