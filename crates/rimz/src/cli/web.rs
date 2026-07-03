//! `rimz web` — Zellij browser access for Rimz rooms.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use super::{GlobalFlags, machine_config};
use crate::cli::room::{self, MissingSessionReport};
use rimz::ids::MuxName;
use rimz::mux::CommandSpec;
use rimz::web::{
    ParsedWebStatus, WebOpenPayload, WebServerStatus, WebStartOptions, WebStatusPayload,
    WebTokenCommand, ZellijWebEndpoint, effective_base_url, endpoint_from_status_base,
    join_session_url, parse_status, parse_token_count, web_help_spec, web_start_spec,
    web_status_spec, web_stop_spec, web_token_spec,
};

#[derive(Debug, Args)]
pub struct WebArgs {
    #[command(subcommand)]
    command: Option<WebSubcmd>,
}

#[derive(Debug, Subcommand)]
enum WebSubcmd {
    /// Open the current workspace's Zellij web URL.
    Open(WebOpenArgs),
    /// Print the current workspace's Zellij web URL without starting the server.
    Url(WebUrlArgs),
    /// Report Zellij web server status.
    Status(WebStatusArgs),
    /// Start the Zellij web server.
    Start(WebStartArgs),
    /// Stop the Zellij web server.
    Stop,
    /// Manage Zellij web login tokens.
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
    /// Existing Rimz session name to open instead of resolving a path.
    #[arg(long, conflicts_with = "path")]
    session: Option<String>,
    /// Print the URL without launching a browser.
    #[arg(long)]
    print: bool,
    /// Do not start `zellij web` when it is offline.
    #[arg(long)]
    no_start: bool,
    /// Emit the versioned `rimz.web.v1` payload.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct WebUrlArgs {
    /// Workspace path. Defaults to the current directory.
    #[arg(value_name = "PATH")]
    path: Option<PathBuf>,
    /// Existing Rimz session name to print instead of resolving a path.
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
        /// Optional token name.
        #[arg(long)]
        name: Option<String>,
    },
    /// List token names and creation dates.
    List,
    /// Revoke one token by name.
    Revoke { name: String },
    /// Revoke all tokens.
    RevokeAll,
}

pub fn run(args: WebArgs, globals: &GlobalFlags) -> Result<()> {
    ensure_zellij_selected(globals)?;
    ensure_zellij_web_available()?;
    match args.command.unwrap_or(WebSubcmd::Open(WebOpenArgs {
        path: None,
        session: None,
        print: false,
        no_start: false,
        json: false,
    })) {
        WebSubcmd::Open(args) => open(args, globals),
        WebSubcmd::Url(args) => url(args, globals),
        WebSubcmd::Status(args) => status(args),
        WebSubcmd::Start(args) => run_inherited(web_start_spec(&WebStartOptions {
            daemonize: args.daemonize,
            ip: args.ip,
            port: args.port,
            cert: args.cert.map(|path| path.display().to_string()),
            key: args.key.map(|path| path.display().to_string()),
        })),
        WebSubcmd::Stop => run_inherited(web_stop_spec()),
        WebSubcmd::Token { command } => run_inherited(web_token_spec(&match command {
            WebTokenSubcmd::Create { read_only, name } => {
                WebTokenCommand::Create { read_only, name }
            }
            WebTokenSubcmd::List => WebTokenCommand::List,
            WebTokenSubcmd::Revoke { name } => WebTokenCommand::Revoke { name },
            WebTokenSubcmd::RevokeAll => WebTokenCommand::RevokeAll,
        })),
    }
}

fn open(args: WebOpenArgs, globals: &GlobalFlags) -> Result<()> {
    let session = if let Some(session) = args.session {
        require_workspace_record_for_session(&session, globals.mux)?;
        session
    } else {
        let path = args.path.unwrap_or_else(|| PathBuf::from("."));
        let workspace = room::ensure_workspace_room_for_web(&path, globals)?;
        workspace.session_name
    };
    let config = machine_config();
    let payload = web_payload(
        &session,
        config.web.zellij.base_url.as_deref(),
        StartPolicy {
            may_start: config.web.zellij.auto_start && !args.no_start,
            require_online: true,
        },
    )?;
    if args.json {
        print_json(&payload)?;
        return Ok(());
    }
    print_url(&payload.url)?;
    if !args.print {
        open_browser_best_effort(&payload.url);
    }
    Ok(())
}

fn url(args: WebUrlArgs, globals: &GlobalFlags) -> Result<()> {
    let session = if let Some(session) = args.session {
        require_workspace_record_for_session(&session, globals.mux)?;
        session
    } else {
        let path = args.path.unwrap_or_else(|| PathBuf::from("."));
        let workspace = rimz::WorkspaceResolver::resolve(&path, globals.root.clone())
            .with_context(|| format!("resolving workspace at {}", path.display()))?;
        let record = room::workspace_record_for_session(&workspace.session_name)
            .context("checking Rimz workspace record")?;
        let Some(record) = record else {
            bail!(
                "workspace session `{}` has not been born by Rimz; run `rimz web open {}` or `rimz start {}` first",
                workspace.session_name,
                path.display(),
                path.display(),
            );
        };
        record.session_name
    };
    let config = machine_config();
    let payload = web_payload(
        &session,
        config.web.zellij.base_url.as_deref(),
        StartPolicy {
            may_start: false,
            require_online: false,
        },
    )?;
    if args.json {
        print_json(&payload)
    } else {
        print_url(&payload.url)
    }
}

fn status(args: WebStatusArgs) -> Result<()> {
    if !args.json {
        return run_inherited(web_status_spec());
    }
    let status = read_web_status()?;
    let token_count = read_token_count()?;
    let base_url = effective_base_url(None, status.base_url.as_deref());
    let endpoint = endpoint_from_status_base(status.base_url.as_deref());
    print_json(&WebStatusPayload::new(
        status.online,
        base_url,
        endpoint,
        token_count,
    ))
}

#[derive(Clone, Copy)]
struct StartPolicy {
    may_start: bool,
    require_online: bool,
}

fn web_payload(
    session: &str,
    configured_base_url: Option<&str>,
    start: StartPolicy,
) -> Result<WebOpenPayload> {
    let mut status = read_web_status()?;
    if !status.online {
        if start.may_start {
            run_captured_to_stderr(web_start_spec(&WebStartOptions {
                daemonize: true,
                ..WebStartOptions::default()
            }))?;
            status = read_web_status()?;
            if !status.online {
                bail!("Zellij web server did not report online after start");
            }
        } else if start.require_online {
            bail!(
                "Zellij web server is offline; run `rimz web start --daemonize` or omit `--no-start`"
            );
        }
    }
    let status_base_url = status.base_url.as_deref();
    let base_url = effective_base_url(configured_base_url, status_base_url);
    let endpoint = endpoint_from_status_base(status_base_url);
    let url = join_session_url(&base_url, session);
    let token_count = read_token_count()?;
    Ok(WebOpenPayload::new(
        url,
        session.to_owned(),
        base_url,
        endpoint,
        token_count,
    ))
}

fn read_web_status() -> Result<WebServerStatus> {
    let output = web_status_spec()
        .output_raw()
        .context("running `zellij web --status`")?;
    if !output.status.success() {
        return Err(command_error(&web_status_spec(), &output.stderr));
    }
    match parse_status(&output.stdout) {
        ParsedWebStatus::Recognized(status) => Ok(status),
        ParsedWebStatus::Unrecognized { raw } => bail!(
            "could not parse `zellij web --status` output; upgrade Rimz or report this output: {raw:?}"
        ),
    }
}

fn read_token_count() -> Result<usize> {
    let spec = web_token_spec(&WebTokenCommand::List);
    let output = spec
        .output_raw()
        .context("running `zellij web --list-tokens`")?;
    if !output.status.success() {
        return Err(command_error(&spec, &output.stderr));
    }
    Ok(parse_token_count(&output.stdout))
}

fn require_workspace_record_for_session(
    session: &str,
    explicit_mux: Option<MuxName>,
) -> Result<()> {
    let record =
        room::workspace_record_for_session(session).context("checking Rimz workspace record")?;
    if record.is_none() {
        bail!(
            "session `{session}` is not a known Rimz workspace session; run `rimz list` or open the workspace with `rimz start` first"
        );
    }
    let mux = room::pick_mux_for_session(session, explicit_mux, MissingSessionReport::Silent)?;
    if mux != MuxName::Zellij {
        bail!("session `{session}` is not a Zellij session; `rimz web` supports Zellij only");
    }
    Ok(())
}

fn ensure_zellij_selected(globals: &GlobalFlags) -> Result<()> {
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    if mux != MuxName::Zellij {
        bail!(
            "`rimz web` supports Zellij only; selected backend is `{mux}`. Use `rimz attach` for tmux rooms, or rerun with `--mux zellij`."
        );
    }
    Ok(())
}

fn ensure_zellij_web_available() -> Result<()> {
    let spec = web_help_spec();
    let output = spec.output_raw().context("checking `zellij web` support")?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!(
        "`zellij web` is unavailable; install Zellij 0.44.3 or newer with web support. zellij said: {}",
        stderr.trim()
    );
}

fn run_inherited(spec: CommandSpec) -> Result<()> {
    let status = spec
        .to_command()
        .status()
        .with_context(|| format!("running `{}`", command_display(&spec)))?;
    if !status.success() {
        bail!("command `{}` exited with {status}", command_display(&spec));
    }
    Ok(())
}

fn run_captured_to_stderr(spec: CommandSpec) -> Result<()> {
    let output = spec
        .output_raw()
        .with_context(|| format!("running `{}`", command_display(&spec)))?;
    let mut stderr = std::io::stderr().lock();
    if !output.stdout.is_empty() {
        stderr.write_all(&output.stdout)?;
    }
    if !output.stderr.is_empty() {
        stderr.write_all(&output.stderr)?;
    }
    if !output.status.success() {
        bail!(
            "command `{}` exited with {}",
            command_display(&spec),
            output.status
        );
    }
    Ok(())
}

fn command_error(spec: &CommandSpec, stderr: &[u8]) -> anyhow::Error {
    anyhow::anyhow!(
        "command `{}` failed: {}",
        command_display(spec),
        String::from_utf8_lossy(stderr).trim()
    )
}

fn command_display(spec: &CommandSpec) -> String {
    if spec.args.is_empty() {
        spec.program.clone()
    } else {
        format!("{} {}", spec.program, spec.args.join(" "))
    }
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}

pub(crate) fn print_url(url: &str) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{url}")?;
    Ok(())
}

pub(crate) fn open_browser_best_effort(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if which::which(opener).is_err() {
        return;
    }
    let _ = Command::new(opener)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

pub(crate) fn local_tunnel_payload(
    remote: &WebOpenPayload,
    local_port: u16,
) -> (String, ZellijWebEndpoint) {
    let base_url = format!("http://127.0.0.1:{local_port}");
    let url = join_session_url(&base_url, &remote.session);
    (
        url,
        ZellijWebEndpoint {
            ip: "127.0.0.1".to_owned(),
            port: local_port,
        },
    )
}
