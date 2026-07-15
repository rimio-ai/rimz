//! `rimz web` — browser access for RimZ rooms.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use super::{GlobalFlags, machine_config, open_browser_best_effort};
use crate::cli::room;
use rimz::ids::MuxName;
use rimz::mux::CommandSpec;
use rimz::web::{
    CredentialCommand, CredentialOutcome, WebCredential, WebEngine, WebStartOptions, WebWarning,
};

#[derive(Debug, Args)]
pub struct WebArgs {
    #[command(subcommand)]
    command: Option<WebSubcmd>,
}

#[derive(Debug, Subcommand)]
enum WebSubcmd {
    /// Open the current workspace's web URL.
    Open(WebOpenArgs),
    /// Print the current workspace's web URL without starting the server.
    Url(WebUrlArgs),
    /// Report browser-access server status.
    Status(WebStatusArgs),
    /// Start the Zellij web server.
    Start(WebStartArgs),
    /// Stop browser-access servers.
    Stop,
    /// Manage browser-access credentials.
    Token {
        #[command(subcommand)]
        command: WebTokenSubcmd,
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
    /// Do not start the room's web engine when it is offline.
    #[arg(long)]
    no_start: bool,
    /// Come up empty: skip recovering prior agents when the room is reborn.
    #[arg(long)]
    no_resume: bool,
    /// Prompt for resume even when stdin is not a terminal. Internal remote-web helper.
    #[arg(long, hide = true)]
    confirm_resume: bool,
    /// Emit the versioned `rimz.web.v1` payload.
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
    /// Emit the versioned `rimz.web.v1` payload.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct WebStatusArgs {
    /// Emit JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct WebStartArgs {
    #[arg(long)]
    daemonize: bool,
    #[arg(long)]
    ip: Option<String>,
    #[arg(long)]
    port: Option<u16>,
    #[arg(long)]
    cert: Option<PathBuf>,
    #[arg(long)]
    key: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum WebTokenSubcmd {
    /// Create a login token.
    Create {
        /// Create a read-only watcher token.
        #[arg(long)]
        read_only: bool,
    },
    /// List token names and creation dates.
    List,
    /// Revoke one token by name.
    Revoke { name: String },
    /// Revoke all tokens.
    RevokeAll,
    /// Provision the machine's login token if needed. Internal remote-web helper.
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
        WebSubcmd::Start(args) => start(args, globals),
        WebSubcmd::Stop => stop(),
        WebSubcmd::Token { command } => token(command, globals),
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
    let mux = context.mux_name();
    ensure_session_addressable_for_web(mux, &session)?;
    let engine = WebEngine::from(mux);
    let outcome = engine.open_session(&session, &config, !args.no_start)?;
    write_warnings(&outcome.warnings);
    if mux == MuxName::Zellij {
        if context.share_web() {
            warn_if_web_sharing_unconfirmed(&session);
        } else {
            warn_web_sharing_unconfirmed(&session);
        }
    }
    if args.json {
        return print_json(&outcome.payload);
    }
    print_url(&outcome.payload.url)?;
    match outcome
        .credential
        .map_or_else(|| engine.ensure_credential(), std::result::Result::Ok)
    {
        Ok(credential) => write_web_credential(&credential),
        Err(err) => write_credential_error(&err),
    }
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

fn warn_if_web_sharing_unconfirmed(session: &str) {
    let cache_root = rimz::store::paths::cache_home();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if rimz::mux::recovery::zellij_session_web_clients_allowed_in(&cache_root, session)
            == Some(true)
        {
            return;
        }
        if Instant::now() >= deadline {
            warn_web_sharing_unconfirmed(session);
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn warn_web_sharing_unconfirmed(session_name: &str) {
    let _ = writeln!(
        std::io::stderr().lock(),
        "rimz: could not confirm Zellij web sharing for `{session_name}`; if the browser says \"Web clients are not allowed to attach to this session\", check that Zellij is new enough, RimZ's presence plugin is available, and `[web] enabled = true` in `rimz config path`, then rerun `rimz web open`."
    );
}

fn url(args: WebUrlArgs, globals: &GlobalFlags) -> Result<()> {
    let context = if let Some(session) = args.session {
        room::web_room_for_session(&session, globals)?
    } else {
        let path = args.path.unwrap_or_else(|| PathBuf::from("."));
        room::existing_web_room_for_path(&path, globals)?
    };
    let payload = WebEngine::from(context.mux_name())
        .inspect_session(context.session_name(), &machine_config())?;
    if args.json {
        print_json(&payload)
    } else {
        print_url(&payload.url)
    }
}

fn status(args: WebStatusArgs) -> Result<()> {
    let report = rimz::web::status()?;
    if args.json {
        return print_json(&report.payload);
    }
    let mut stdout = std::io::stdout().lock();
    if report.zellij_available {
        writeln!(
            stdout,
            "zellij: {}",
            if report.payload.online {
                "online"
            } else {
                "offline"
            }
        )?;
    } else {
        writeln!(stdout, "zellij: unavailable")?;
    }
    if report.payload.tmux_instances.is_empty() {
        writeln!(stdout, "ttyd: no live tmux instances")?;
    } else {
        for instance in report.payload.tmux_instances {
            writeln!(
                stdout,
                "ttyd: {} on 127.0.0.1:{} (pid {})",
                instance.session, instance.port, instance.pid
            )?;
        }
    }
    Ok(())
}

fn start(args: WebStartArgs, globals: &GlobalFlags) -> Result<()> {
    if rimz::mux::auto_detect_backend(globals.mux)? == MuxName::Tmux {
        bail!("ttyd serves tmux rooms per room; run `rimz web open`");
    }
    let prepared = rimz::web::prepare_zellij_start(
        &machine_config(),
        WebStartOptions {
            daemonize: args.daemonize,
            ip: args.ip,
            port: args.port,
            cert: args.cert.map(|path| path.display().to_string()),
            key: args.key.map(|path| path.display().to_string()),
        },
    )?;
    write_warnings(&prepared.warnings);
    run_inherited(prepared.command)
}

fn stop() -> Result<()> {
    let stopped = rimz::web::stop_all()?;
    let zellij_summary = if stopped.zellij_stopped == 1 {
        "1 Zellij server"
    } else {
        "0 Zellij servers"
    };
    let ttyd_noun = if stopped.ttyd_stopped == 1 {
        "ttyd instance"
    } else {
        "ttyd instances"
    };
    writeln!(
        std::io::stdout().lock(),
        "stopped {zellij_summary} and {} {ttyd_noun}",
        stopped.ttyd_stopped
    )?;
    Ok(())
}

fn token(command: WebTokenSubcmd, globals: &GlobalFlags) -> Result<()> {
    let engine = WebEngine::from(rimz::mux::auto_detect_backend(globals.mux)?);
    render_credential_outcome(engine.credential(command.into())?)
}

fn render_credential_outcome(outcome: CredentialOutcome) -> Result<()> {
    match outcome {
        CredentialOutcome::Raw(output) => {
            std::io::stdout().lock().write_all(&output.stdout)?;
            std::io::stderr().lock().write_all(&output.stderr)?;
        }
        CredentialOutcome::Ensured(credential) => {
            writeln!(std::io::stdout().lock(), "{}", credential.secret())?;
        }
        CredentialOutcome::Rotated {
            credential,
            restarted_instances,
        } => {
            write_web_credential(&credential);
            writeln!(
                std::io::stdout().lock(),
                "rotated ttyd credential and restarted {restarted_instances} instance(s)"
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
                "revoked ttyd credential and stopped {stopped_instances} instance(s)"
            )?;
        }
    }
    Ok(())
}

fn write_web_credential(credential: &WebCredential) {
    let mut stderr = std::io::stderr().lock();
    match credential {
        WebCredential::ZellijLogin { secret } => {
            let _ = writeln!(
                stderr,
                "Zellij web login token (paste into the browser's \"Security Token Required\" page): {secret}"
            );
        }
        WebCredential::BasicAuth { username, secret } => {
            let _ = writeln!(
                stderr,
                "ttyd basic auth for this machine (browser will show a Basic-Auth prompt): user {username}, password {secret}"
            );
        }
    }
}

fn write_credential_error(err: &rimz::web::WebErr) {
    let _ = writeln!(
        std::io::stderr().lock(),
        "rimz: could not mint a Zellij web login token ({err}); create one with `rimz web token create`."
    );
}

fn write_warnings(warnings: &[WebWarning]) {
    let mut stderr = std::io::stderr().lock();
    for warning in warnings {
        match warning {
            WebWarning::BrowserThemeSkipped(detail) => {
                let _ = writeln!(stderr, "rimz: skipping browser theme: {detail}");
            }
        }
    }
}

fn run_inherited(spec: CommandSpec) -> Result<()> {
    let status = spec
        .to_command()
        .status()
        .with_context(|| format!("running `{}`", spec.display_line()))?;
    if !status.success() {
        bail!("command `{}` exited with {status}", spec.display_line());
    }
    Ok(())
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}

fn print_url(url: &str) -> Result<()> {
    writeln!(std::io::stdout().lock(), "{url}")?;
    Ok(())
}
