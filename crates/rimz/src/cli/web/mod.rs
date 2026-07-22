//! `rimz web` — writable browser access and read-only room broadcasts.

mod picker;

use std::io::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use super::{GlobalFlags, machine_config, open_browser_best_effort};
use crate::cli::room;
use rimz::web::{WebAuth, WebCredential};

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
    /// Share one live room as an unauthenticated read-only broadcast.
    Share(WebShareArgs),
    /// Stop sharing one room or every room.
    Unshare(WebUnshareArgs),
    /// Report the shared ttyd daemon's status.
    Status(WebStatusArgs),
    /// Start the shared ttyd daemon.
    Start,
    /// Restart the shared ttyd daemon and apply the current config.
    Restart,
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
        #[arg(long)]
        share: bool,
    },
    /// Restrict and forward connections to the shared ttyd daemon.
    #[command(hide = true)]
    Gate {
        #[arg(long)]
        listen: SocketAddr,
        #[arg(long)]
        upstream: SocketAddr,
        #[arg(long = "allow")]
        allow: Vec<String>,
        #[arg(long)]
        auth_header: Option<String>,
        #[arg(long = "auth-user")]
        auth_users: Vec<String>,
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
struct WebShareArgs {
    /// Workspace path. Defaults to the current directory.
    #[arg(value_name = "PATH")]
    path: Option<PathBuf>,
    /// Existing live RimZ session to share instead of resolving a path.
    #[arg(long, conflicts_with = "path")]
    session: Option<String>,
    /// Print the URL without launching a browser.
    #[arg(long)]
    print: bool,
    /// Emit the versioned `rimz.web.share.v1` payload.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct WebUnshareArgs {
    /// Workspace path. Defaults to the current directory.
    #[arg(value_name = "PATH", conflicts_with = "session")]
    path: Option<PathBuf>,
    /// RimZ session name to stop sharing.
    #[arg(long)]
    session: Option<String>,
    /// Stop sharing every room.
    #[arg(long, conflicts_with_all = ["path", "session"])]
    all: bool,
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
        WebSubcmd::Share(args) => share(args, globals),
        WebSubcmd::Unshare(args) => unshare(args, globals),
        WebSubcmd::Status(args) => status(args),
        WebSubcmd::Start => start(),
        WebSubcmd::Restart => restart(),
        WebSubcmd::Stop => stop(),
        WebSubcmd::Token { command } => token(command),
        WebSubcmd::Exec { session, share } => exec(session.as_deref(), share),
        WebSubcmd::Gate {
            listen,
            upstream,
            allow,
            auth_header,
            auth_users,
        } => {
            let auth = auth_header
                .map(|header_name| -> rimz::web::Result<_> {
                    Ok(rimz::web::GateAuth {
                        header_name,
                        allowed_users: auth_users,
                        authorization: rimz::web::gate_authorization()?,
                    })
                })
                .transpose()?;
            rimz::web::serve_gate(listen, upstream, &allow, auth).map_err(Into::into)
        }
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
    rimz::web::ensure_session_addressable(context.mux_name(), &session)?;
    let outcome = rimz::web::open_session(&session, &config, !args.no_start)?;
    crate::cli::render::web_warnings(&outcome.warnings);
    if args.json {
        return crate::cli::render::json(&outcome.payload);
    }
    print_url(&outcome.payload.url)?;
    if let WebAuth::TrustedHeader { header } = &outcome.payload.auth {
        let _ = writeln!(
            std::io::stderr().lock(),
            "rimz: authentication is delegated to the reverse proxy (trusted header `{header}`)"
        );
    }
    write_web_credential(
        outcome
            .payload
            .credential
            .as_ref()
            .context("shared ttyd daemon returned no credential")?,
    );
    if !args.print {
        open_browser_best_effort(&outcome.payload.url);
    }
    Ok(())
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

fn share(args: WebShareArgs, globals: &GlobalFlags) -> Result<()> {
    let config = machine_config();
    ensure_web_enabled(&config)?;
    let context = if let Some(session) = args.session {
        room::web_room_for_session(&session, globals)?
    } else {
        let path = args.path.unwrap_or_else(|| PathBuf::from("."));
        room::existing_web_room_for_path(&path, globals)?
    };
    let outcome = rimz::web::share_session(context.session_name(), &config)?;
    crate::cli::render::web_warnings(&outcome.warnings);
    if args.json {
        return crate::cli::render::json(&outcome.payload);
    }
    print_url(&outcome.payload.url)?;
    if !args.print {
        open_browser_best_effort(&outcome.payload.url);
    }
    Ok(())
}

fn unshare(args: WebUnshareArgs, globals: &GlobalFlags) -> Result<()> {
    let config = machine_config();
    if args.all {
        let outcome = rimz::web::unshare_all(&config)?;
        writeln!(
            std::io::stdout().lock(),
            "{}",
            if outcome.changed {
                "stopped sharing all rooms"
            } else {
                "no rooms were shared"
            }
        )?;
        return Ok(());
    }
    let session = if let Some(session) = args.session {
        session
    } else {
        let path = args.path.unwrap_or_else(|| PathBuf::from("."));
        rimz::WorkspaceResolver::resolve(&path, globals.root.clone())
            .with_context(|| format!("resolving workspace at {}", path.display()))?
            .session_name
    };
    let outcome = rimz::web::unshare_session(&session, &config)?;
    if let Some(daemon) = &outcome.daemon {
        crate::cli::render::web_warnings(&daemon.warnings);
    }
    writeln!(
        std::io::stdout().lock(),
        "{}",
        if outcome.changed {
            format!("stopped sharing `{session}`")
        } else {
            format!("`{session}` was not shared")
        }
    )?;
    Ok(())
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
            "ttyd: online on {} (pid {pid})",
            listener_display(&status.interface, status.port)
        )?;
    } else {
        writeln!(
            stdout,
            "ttyd: offline (configured listener {})",
            listener_display(&status.interface, status.port)
        )?;
    }
    if let Some(pid) = status.share.pid {
        let sessions = if status.share.sessions.is_empty() {
            "(none)".to_owned()
        } else {
            status.share.sessions.join(", ")
        };
        writeln!(
            stdout,
            "share: online on {} (pid {pid}), sharing: {sessions}",
            listener_display(&status.share.interface, status.share.port)
        )?;
    } else {
        writeln!(
            stdout,
            "share: offline (configured listener {}), sharing: {}",
            listener_display(&status.share.interface, status.share.port),
            if status.share.sessions.is_empty() {
                "(none)".to_owned()
            } else {
                status.share.sessions.join(", ")
            }
        )?;
    }
    Ok(())
}

fn start() -> Result<()> {
    let outcome = rimz::web::ensure_daemon(&machine_config())?;
    crate::cli::render::web_warnings(&outcome.warnings);
    writeln!(
        std::io::stdout().lock(),
        "ttyd: online on {} (pid {})",
        listener_display(&outcome.interface, outcome.port),
        outcome.pid
    )?;
    Ok(())
}

fn restart() -> Result<()> {
    let outcome = rimz::web::restart_daemon(&machine_config())?;
    crate::cli::render::web_warnings(&outcome.warnings);
    writeln!(
        std::io::stdout().lock(),
        "ttyd: online on {} (pid {})",
        listener_display(&outcome.interface, outcome.port),
        outcome.pid
    )?;
    if !outcome.was_online {
        writeln!(
            std::io::stdout().lock(),
            "ttyd: was offline; started a fresh daemon"
        )?;
    }
    if let Some(share) = outcome.share {
        crate::cli::render::web_warnings(&share.warnings);
        writeln!(
            std::io::stdout().lock(),
            "share: online on {} (pid {})",
            listener_display(&share.interface, share.port),
            share.pid
        )?;
    }
    Ok(())
}

fn stop() -> Result<()> {
    let stopped = rimz::web::stop_daemons()?;
    writeln!(
        std::io::stdout().lock(),
        "stopped {} ttyd daemon{}",
        stopped,
        if stopped == 1 { "" } else { "s" }
    )?;
    Ok(())
}

fn token(command: WebTokenSubcmd) -> Result<()> {
    match command {
        WebTokenSubcmd::Create { read_only } => {
            let outcome = rimz::web::rotate_credential(&machine_config(), read_only)?;
            let restarted = usize::from(outcome.restarted);
            crate::cli::render::web_warnings(&outcome.warnings);
            write_web_credential(&outcome.credential);
            writeln!(
                std::io::stdout().lock(),
                "rotated ttyd credential and restarted {restarted} daemon(s)"
            )?;
        }
        WebTokenSubcmd::List => {
            if let Some(credential) = rimz::web::credential_summary()? {
                let mut stdout = std::io::stdout().lock();
                writeln!(stdout, "{}: {}", credential.name, credential.created_at)?;
            }
        }
        WebTokenSubcmd::Revoke { name } => {
            render_revoked(rimz::web::revoke_credential(Some(&name))?)?;
        }
        WebTokenSubcmd::RevokeAll => {
            render_revoked(rimz::web::revoke_credential(None)?)?;
        }
    }
    Ok(())
}

fn render_revoked(stopped: bool) -> Result<()> {
    let stopped = usize::from(stopped);
    writeln!(
        std::io::stdout().lock(),
        "revoked ttyd credential and stopped {stopped} daemon(s)"
    )?;
    Ok(())
}

fn exec(session: Option<&str>, share: bool) -> Result<()> {
    if share {
        let spec = rimz::web::share_attach_command(session)?;
        return exec_session_attach(&spec, session);
    }
    match rimz::web::existing_session_attach_command(session) {
        Ok(spec) if picker::available() => {
            let Some(session) = session.filter(|session| !session.is_empty()) else {
                return exec_session_attach(&spec, session);
            };
            if picker::run(None, Some((session, &spec)))? {
                Ok(())
            } else {
                exec_session_attach(&spec, Some(session))
            }
        }
        Ok(spec) => exec_session_attach(&spec, session),
        Err(rimz::web::WebErr::InvalidSession(message)) if picker::available() => {
            if picker::run(session, None)? {
                Ok(())
            } else {
                Err(anyhow::anyhow!(message))
            }
        }
        Err(err) => Err(err.into()),
    }
}

fn exec_session_attach(spec: &rimz::mux::CommandSpec, session: Option<&str>) -> Result<()> {
    let session = session
        .filter(|session| !session.is_empty())
        .context("validated browser attach lost its session target")?;
    let display_name = picker::session_display_name(session);
    picker::write_session_sync(Some((session, &display_name)))?;
    room::exec_attach_command(spec)
}

fn ensure_web_enabled(config: &rimz::config::MachineConfig) -> Result<()> {
    if !config.web.enabled {
        bail!(
            "Browser access is disabled: set `[web] enabled = true` in the RimZ config on the machine serving this room (`rimz config path`) to allow browser sharing."
        );
    }
    Ok(())
}

fn write_web_credential(credential: &WebCredential) {
    let _ = writeln!(
        std::io::stderr().lock(),
        "ttyd basic auth for this machine: user {}, password {}",
        credential.username,
        credential.secret
    );
}

fn print_url(url: &str) -> Result<()> {
    writeln!(std::io::stdout().lock(), "{url}")?;
    Ok(())
}

fn listener_display(interface: &str, port: u16) -> String {
    interface.parse::<std::net::IpAddr>().map_or_else(
        |_| format!("{interface}:{port}"),
        |ip| SocketAddr::new(ip, port).to_string(),
    )
}
