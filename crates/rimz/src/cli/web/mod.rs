//! `rimz web` — writable browser access and read-only room broadcasts.

use std::io::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use super::{GlobalFlags, machine_config, open_browser};
use crate::cli::room;
use rimz::web::{TokenDaemonChange, WebAuth};

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
    /// Show whether the browser daemons are up, their URLs, and who can sign in.
    Status(WebStatusArgs),
    /// Start the writable browser daemon.
    Start,
    /// Restart the browser daemons and apply the current config.
    Restart,
    /// Stop both browser daemons (the share list is kept).
    Stop,
    /// Manage the browser sign-in tokens (each is a Basic-auth user and password).
    #[command(after_help = "Read-only viewing is a per-room broadcast: see `rimz web share`.")]
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
    /// Check sign-in and forward connections to ttyd.
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
    /// Require the writable daemon to already be online.
    #[arg(long)]
    no_start: bool,
    /// Come up empty: skip recovering prior agents when the room is reborn.
    #[arg(long)]
    no_resume: bool,
    /// Prompt for resume even when stdin is not a terminal. Internal remote-web helper.
    #[arg(long, hide = true)]
    confirm_resume: bool,
    /// Emit the rimz.web.v2 payload, including the credential the tunnel relay presents to ttyd.
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
    /// Emit the rimz.web.url.v1 payload, without credentials.
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
    /// Mint a token named NAME and restart the writable daemon.
    Create { name: String },
    /// Print a token's password (default: rimz).
    Show {
        #[arg(default_value = "rimz")]
        name: String,
    },
    /// List tokens and when each was created.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Delete a token and restart the writable daemon so its sessions end; the last token stops the daemon.
    Revoke {
        /// Token name, as listed by `rimz web token list`.
        name: String,
    },
    /// Delete every token and stop the writable daemon.
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
        WebSubcmd::Exec { session, share } => exec(session.as_deref(), share, globals),
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
    ensure_web_enabled(&config)?;
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
    let sign_in = match &outcome.payload.auth {
        WebAuth::TrustedHeader { header } => {
            format!("sign in through your reverse proxy (header {header})")
        }
        WebAuth::Basic => {
            let names = rimz::web::tokens()?
                .tokens
                .into_iter()
                .map(|token| token.name)
                .collect::<Vec<_>>();
            if let [name] = names.as_slice() {
                format!("sign in as user {name}; password: rimz web token show {name}")
            } else {
                format!(
                    "sign in as one of {}; password: rimz web token show <name>",
                    names.join(", ")
                )
            }
        }
    };
    let _ = writeln!(std::io::stderr().lock(), "rimz: {sign_in}");
    if !args.print {
        open_browser(&outcome.payload.url);
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
        open_browser(&outcome.payload.url);
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
            match outcome.sessions.len() {
                0 => format!("stopped sharing `{session}`; the broadcast daemon is stopped"),
                count => format!(
                    "stopped sharing `{session}`; viewers of {count} other shared room{} were disconnected and will reconnect",
                    if count == 1 { "" } else { "s" }
                ),
            }
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
    let mut stdout = crate::cli::render::out();
    let writable = &status.writable;
    let sign_in = match &writable.auth {
        WebAuth::TrustedHeader { header } => format!("reverse proxy (header {header})"),
        WebAuth::Basic => match writable.tokens.as_slice() {
            [] => "no token (rimz web token create <name>)".to_owned(),
            [name] => format!("tokens {name} (rimz web token show {name})"),
            names => format!("tokens {} (rimz web token show <name>)", names.join(", ")),
        },
    };
    writeln!(
        stdout,
        "writable   {:<7}   {}   sign-in: {sign_in}",
        if writable.online { "online" } else { "offline" },
        writable.url
    )?;
    let listener = listener_display(&writable.interface, writable.port);
    if let Some(pid) = writable.pid {
        let gate = writable
            .gate_pid
            .map(|pid| format!("gate pid {pid}, "))
            .unwrap_or_default();
        writeln!(
            stdout,
            "                     listening on {listener} ({gate}ttyd pid {pid})"
        )?;
    } else {
        writeln!(stdout, "                     would listen on {listener}")?;
    }
    let broadcast = &status.broadcast;
    let rooms = match broadcast.sessions.len() {
        0 => "no rooms shared".to_owned(),
        count => format!(
            "{count} room{} shared: {}",
            if count == 1 { "" } else { "s" },
            broadcast.sessions.join(", ")
        ),
    };
    let url = if broadcast.online {
        format!("   {}", broadcast.url)
    } else {
        String::new()
    };
    writeln!(
        stdout,
        "broadcast  {:<7}   {rooms}{url}",
        if broadcast.online {
            "online"
        } else {
            "offline"
        }
    )?;
    let listener = listener_display(&broadcast.interface, broadcast.port);
    if let Some(pid) = broadcast.pid {
        writeln!(
            stdout,
            "                     listening on {listener} with no password, pid {pid}"
        )?;
    } else {
        writeln!(
            stdout,
            "                     would listen on {listener} with no password"
        )?;
    }
    Ok(())
}

fn write_writable_online(
    url: &str,
    interface: &str,
    port: u16,
    pid: u32,
    gate_pid: Option<u32>,
) -> Result<()> {
    let gate = gate_pid
        .map(|pid| format!("gate pid {pid}, "))
        .unwrap_or_default();
    writeln!(
        crate::cli::render::out(),
        "writable daemon online at {url} (listening on {}, {gate}ttyd pid {pid})",
        listener_display(interface, port)
    )?;
    Ok(())
}

fn start() -> Result<()> {
    let config = machine_config();
    ensure_web_enabled(&config)?;
    let outcome = rimz::web::ensure_daemon(&config)?;
    crate::cli::render::web_warnings(&outcome.warnings);
    write_writable_online(
        &outcome.url,
        &outcome.interface,
        outcome.port,
        outcome.pid,
        outcome.gate_pid,
    )
}

fn restart() -> Result<()> {
    let config = machine_config();
    ensure_web_enabled(&config)?;
    let outcome = rimz::web::restart_daemon(&config)?;
    crate::cli::render::web_warnings(&outcome.warnings);
    write_writable_online(
        &outcome.url,
        &outcome.interface,
        outcome.port,
        outcome.pid,
        outcome.gate_pid,
    )?;
    if !outcome.was_online {
        writeln!(
            crate::cli::render::out(),
            "writable daemon was offline; started fresh"
        )?;
    }
    if let Some(share) = outcome.share {
        crate::cli::render::web_warnings(&share.warnings);
        writeln!(
            crate::cli::render::out(),
            "broadcast daemon online at {} (pid {})",
            share.url,
            share.pid
        )?;
    }
    Ok(())
}

fn stop() -> Result<()> {
    let stopped = rimz::web::stop_daemons()?;
    let message = match (stopped.writable, stopped.broadcast) {
        (true, true) => {
            "stopped the writable daemon and the broadcast daemon; the share list is kept"
        }
        (true, false) => "stopped the writable daemon",
        (false, true) => "stopped the broadcast daemon; the share list is kept",
        (false, false) => "no browser daemon was running",
    };
    writeln!(crate::cli::render::out(), "{message}")?;
    Ok(())
}

fn token(command: WebTokenSubcmd) -> Result<()> {
    match command {
        WebTokenSubcmd::Create { name } => {
            let outcome = rimz::web::create_token(&machine_config(), &name)?;
            crate::cli::render::web_warnings(&outcome.warnings);
            writeln!(
                std::io::stderr().lock(),
                "rimz: token {name}: password {}",
                outcome.credential.secret
            )?;
            writeln!(
                crate::cli::render::out(),
                "created token {name}{}",
                if outcome.restarted {
                    " and restarted the writable daemon"
                } else {
                    "; the writable daemon is offline"
                }
            )?;
        }
        WebTokenSubcmd::Show { name } => {
            writeln!(
                std::io::stdout().lock(),
                "{}",
                rimz::web::token_credential(&name)?.secret
            )?;
        }
        WebTokenSubcmd::List { json } => {
            let payload = rimz::web::tokens()?;
            if json {
                return crate::cli::render::json(&payload);
            }
            let mut stdout = crate::cli::render::out();
            if payload.tokens.is_empty() {
                writeln!(stdout, "no tokens; run rimz web token create <name>")?;
            }
            for token in payload.tokens {
                let age = (jiff::Timestamp::now().as_second() - token.created_at.as_second())
                    .max(0)
                    .unsigned_abs();
                writeln!(
                    stdout,
                    "{}  created {} ({} ago)",
                    token.name,
                    rimz::utils::time::format_local_timestamp(token.created_at),
                    rimz::utils::time::format_duration_compact(std::time::Duration::from_secs(age))
                )?;
            }
        }
        WebTokenSubcmd::Revoke { name } => {
            render_revoked(
                Some(&name),
                rimz::web::revoke_token(&machine_config(), Some(&name))?,
            )?;
        }
        WebTokenSubcmd::RevokeAll => {
            render_revoked(None, rimz::web::revoke_token(&machine_config(), None)?)?;
        }
    }
    Ok(())
}

fn render_revoked(name: Option<&str>, outcome: rimz::web::TokenRevoked) -> Result<()> {
    crate::cli::render::web_warnings(&outcome.warnings);
    if outcome.count == 0 {
        writeln!(crate::cli::render::out(), "no tokens to revoke")?;
        return Ok(());
    }
    let subject = name.map_or_else(
        || format!("{} tokens", outcome.count),
        |name| format!("token {name}"),
    );
    let change = match outcome.daemon {
        TokenDaemonChange::Offline => "; the writable daemon is offline",
        TokenDaemonChange::Restarted => " and restarted the writable daemon",
        TokenDaemonChange::RestartFailed => " and stopped the writable daemon (restart failed)",
        TokenDaemonChange::Stopped if name.is_some() => {
            " and stopped the writable daemon (no tokens remain)"
        }
        TokenDaemonChange::Stopped => " and stopped the writable daemon",
    };
    writeln!(crate::cli::render::out(), "revoked {subject}{change}")?;
    Ok(())
}

fn exec(session: Option<&str>, share: bool, globals: &GlobalFlags) -> Result<()> {
    if share {
        let spec = rimz::web::share_attach_command(session)?;
        return exec_session_attach(&spec, session);
    }
    match rimz::web::existing_session_attach_command(session) {
        Ok(spec) if crate::cli::sessions::picker_available() => {
            let Some(session) = session.filter(|session| !session.is_empty()) else {
                return exec_session_attach(&spec, session);
            };
            if crate::cli::sessions::run_web_picker(None, Some((session, &spec)), globals)? {
                Ok(())
            } else {
                exec_session_attach(&spec, Some(session))
            }
        }
        Ok(spec) => exec_session_attach(&spec, session),
        Err(rimz::web::WebErr::InvalidSession(message))
            if crate::cli::sessions::picker_available() =>
        {
            if crate::cli::sessions::run_web_picker(session, None, globals)? {
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
    let display_name = crate::cli::sessions::session_display_name(session);
    rimz::web::write_session_sync(Some((session, &display_name)))?;
    room::launch_attach_command(spec)
}

fn ensure_web_enabled(config: &rimz::config::MachineConfig) -> Result<()> {
    if !config.web.enabled {
        bail!(
            "Browser access is disabled: set `[web] enabled = true` in the RimZ config on the machine serving this room (`rimz config path`) to allow browser sharing."
        );
    }
    Ok(())
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
