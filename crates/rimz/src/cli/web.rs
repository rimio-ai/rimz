//! `rimz web` — Zellij browser access for Rimz rooms.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use super::{GlobalFlags, machine_config};
use crate::cli::room;
use rimz::config::MachineConfig;
use rimz::ids::MuxName;
use rimz::ledger::{atomic, paths};
use rimz::mux::CommandSpec;
use rimz::sidebar_pane::render::scheme;
use rimz::web::{
    ParsedWebStatus, WebClientColors, WebOpenPayload, WebServerStatus, WebStartOptions,
    WebStatusPayload, WebTokenCommand, ZellijWebEndpoint, active_zellij_config_path,
    cache_login_token, clear_cached_login_token, effective_base_url, endpoint_from_status_base,
    join_session_url, merge_web_client_config, parse_created_login_token, parse_status,
    parse_token_count, read_cached_login_token, web_help_spec, web_start_spec, web_status_spec,
    web_stop_spec, web_token_spec,
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

pub fn run(args: WebArgs, globals: &GlobalFlags) -> Result<()> {
    ensure_zellij_selected(globals)?;
    ensure_zellij_web_available()?;
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
        WebSubcmd::Start(args) => {
            let config = machine_config();
            let config_file = web_client_config_file(&config);
            run_inherited(web_start_spec(&WebStartOptions {
                daemonize: args.daemonize,
                ip: args.ip,
                port: args.port,
                cert: args.cert.map(|path| path.display().to_string()),
                key: args.key.map(|path| path.display().to_string()),
                config_file,
            }))
        }
        WebSubcmd::Stop => run_inherited(web_stop_spec()),
        WebSubcmd::Token { command } => match command {
            WebTokenSubcmd::Create { read_only } => {
                run_inherited(web_token_spec(&WebTokenCommand::Create { read_only }))
            }
            WebTokenSubcmd::List => run_inherited(web_token_spec(&WebTokenCommand::List)),
            WebTokenSubcmd::Revoke { name } => {
                run_inherited(web_token_spec(&WebTokenCommand::Revoke { name }))?;
                clear_cached_login_token()?;
                Ok(())
            }
            WebTokenSubcmd::RevokeAll => {
                run_inherited(web_token_spec(&WebTokenCommand::RevokeAll))?;
                clear_cached_login_token()?;
                Ok(())
            }
            WebTokenSubcmd::Ensure => run_token_ensure(),
        },
    }
}

fn open(args: WebOpenArgs, globals: &GlobalFlags) -> Result<()> {
    let config = machine_config();
    if !config.web.enabled {
        bail!(
            "Zellij web access is disabled: set `[web] enabled = true` in the Rimz config on the machine serving this room (`rimz config path`) to allow browser sharing."
        );
    }
    let web_room = if let Some(session) = args.session {
        room::ensure_session_room_for_web(&session, globals, args.no_resume, args.confirm_resume)?
    } else {
        let path = args.path.unwrap_or_else(|| PathBuf::from("."));
        room::ensure_workspace_room_for_web(&path, globals, args.no_resume, args.confirm_resume)?
    };
    let room::WebRoom {
        session_name: session,
        workspace_id,
    } = web_room;
    ensure_session_addressable_for_web(&session)?;
    let payload = web_payload(
        &session,
        config.web.zellij.base_url.as_deref(),
        StartPolicy {
            may_start: config.web.zellij.auto_start && !args.no_start,
            require_online: true,
        },
        Some(&config),
    )?;
    let backend = rimz::mux::backend_for(MuxName::Zellij);
    if room::enable_web_sharing(
        backend.as_ref(),
        &session,
        &workspace_id,
        &config.zellij,
        config.web.enabled,
        config.sidebar.focus_key_label(),
    ) {
        warn_if_web_sharing_unconfirmed(&session);
    }
    if args.json {
        print_json(&payload)?;
        return Ok(());
    }
    print_url(&payload.url)?;
    write_login_token_outcome(ensure_login_token());
    if !args.print {
        open_browser_best_effort(&payload.url);
    }
    Ok(())
}

enum LoginTokenOutcome {
    Token(String),
    Failed(String),
}

fn ensure_login_token() -> LoginTokenOutcome {
    match read_cached_login_token() {
        Ok(Some(token)) => return LoginTokenOutcome::Token(token),
        Ok(None) => {}
        Err(err) => return LoginTokenOutcome::Failed(err.to_string()),
    }
    let create = web_token_spec(&WebTokenCommand::Create { read_only: false });
    let output = match create.output_raw() {
        Ok(output) if output.status.success() => output,
        Ok(output) => return LoginTokenOutcome::Failed(command_output_detail(&output)),
        Err(err) => return LoginTokenOutcome::Failed(err.to_string()),
    };
    let token = match parse_created_login_token(&output.stdout) {
        Ok(token) => token,
        Err(err) => return LoginTokenOutcome::Failed(err.to_string()),
    };
    if let Err(err) = cache_login_token(&token) {
        return LoginTokenOutcome::Failed(err.to_string());
    }
    LoginTokenOutcome::Token(token)
}

fn run_token_ensure() -> Result<()> {
    match ensure_login_token() {
        LoginTokenOutcome::Token(token) => {
            let mut stdout = std::io::stdout().lock();
            writeln!(stdout, "{token}")?;
            Ok(())
        }
        LoginTokenOutcome::Failed(detail) => bail!("{detail}"),
    }
}

fn write_login_token_outcome(outcome: LoginTokenOutcome) {
    let mut stderr = std::io::stderr().lock();
    match outcome {
        LoginTokenOutcome::Token(token) => {
            let _ = writeln!(
                stderr,
                "Zellij web login token (paste into the browser's \"Security Token Required\" page): {token}"
            );
        }
        LoginTokenOutcome::Failed(detail) => {
            let _ = writeln!(
                stderr,
                "rimz: could not mint a Zellij web login token ({detail}); create one with `rimz web token create`."
            );
        }
    }
}

fn ensure_session_addressable_for_web(session: &str) -> Result<()> {
    let backend = rimz::mux::backend_for(MuxName::Zellij);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match backend.list_sessions() {
            Ok(sessions) if sessions.iter().any(|name| name == session) => return Ok(()),
            Ok(_) => {
                if Instant::now() >= deadline {
                    bail!(
                        "Zellij session `{session}` is not addressable after web preparation. Run `rimz reset` from the workspace, then retry `rimz web open`."
                    );
                }
            }
            Err(err) => {
                let detail = err.to_string();
                if Instant::now() >= deadline {
                    bail!(
                        "Zellij session `{session}` is not addressable after web preparation: {detail}. Run `rimz reset` from the workspace, then retry `rimz web open`."
                    );
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn warn_if_web_sharing_unconfirmed(session: &str) {
    let cache_root = rimz::ledger::paths::cache_home();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if rimz::mux::recovery::zellij_session_web_clients_allowed_in(&cache_root, session)
            == Some(true)
        {
            return;
        }
        if Instant::now() >= deadline {
            room::warn_web_sharing_unconfirmed(session);
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn url(args: WebUrlArgs, globals: &GlobalFlags) -> Result<()> {
    let session = if let Some(session) = args.session {
        require_workspace_record_for_session(&session)?;
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
        room::ensure_single_backend_room(MuxName::Zellij, &record.session_name)?;
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
        None,
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
    config: Option<&MachineConfig>,
) -> Result<WebOpenPayload> {
    let mut status = read_web_status()?;
    if !status.online {
        if start.may_start {
            let config_file = config.and_then(web_client_config_file);
            run_captured_debug_on_success(web_start_spec(&WebStartOptions {
                daemonize: true,
                config_file,
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

fn web_client_config_file(config: &MachineConfig) -> Option<PathBuf> {
    if !config.web.enabled || !config.web.zellij.style_client {
        return None;
    }
    let colors = match WebClientColors::from_palette(&scheme::resolve_inline_palette(&config.theme))
    {
        Some(colors) => colors,
        None => {
            note_browser_theme_skip("scheme palette is incomplete or malformed");
            return None;
        }
    };
    let existing = match active_zellij_config_path() {
        Some(path) => match std::fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(err) => {
                note_browser_theme_skip(format_args!(
                    "could not read Zellij config `{}`: {err}",
                    path.display()
                ));
                return None;
            }
        },
        None => None,
    };
    let kdl = match merge_web_client_config(existing.as_deref(), &config.web.zellij.font, &colors) {
        Ok(kdl) => kdl,
        Err(err) => {
            note_browser_theme_skip(err);
            return None;
        }
    };
    let path = paths::state_home()
        .join("rimz")
        .join("zellij-web-config.kdl");
    if let Err(err) = atomic::write_bytes_atomically(&path, kdl.as_bytes()) {
        note_browser_theme_skip(format_args!(
            "could not write generated Zellij config `{}`: {err}",
            path.display()
        ));
        return None;
    }
    Some(path)
}

fn note_browser_theme_skip(detail: impl std::fmt::Display) {
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "rimz: skipping browser theme: {detail}");
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

fn require_workspace_record_for_session(session: &str) -> Result<rimz::WorkspaceRecord> {
    let record =
        room::workspace_record_for_session(session).context("checking Rimz workspace record")?;
    let Some(record) = record else {
        bail!(
            "session `{session}` is not a known Rimz workspace session; run `rimz list` or open the workspace with `rimz start` first"
        );
    };
    room::ensure_single_backend_room(MuxName::Zellij, session)?;
    Ok(record)
}

fn ensure_zellij_selected(globals: &GlobalFlags) -> Result<()> {
    if globals.mux == Some(MuxName::Tmux) {
        bail!("`rimz web` supports Zellij only; drop `--mux tmux` (web always uses Zellij).");
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

fn run_captured_debug_on_success(spec: CommandSpec) -> Result<()> {
    let output = spec
        .output_raw()
        .with_context(|| format!("running `{}`", command_display(&spec)))?;
    if output.status.success() {
        if !output.stdout.is_empty() || !output.stderr.is_empty() {
            tracing::debug!(
                command = %command_display(&spec),
                stdout = %String::from_utf8_lossy(&output.stdout).trim(),
                stderr = %String::from_utf8_lossy(&output.stderr).trim(),
                "command output",
            );
        }
        return Ok(());
    }
    bail!(
        "command `{}` exited with {}: {}",
        command_display(&spec),
        output.status,
        command_output_streams(&output)
    )
}

fn command_error(spec: &CommandSpec, stderr: &[u8]) -> anyhow::Error {
    anyhow::anyhow!(
        "command `{}` failed: {}",
        command_display(spec),
        String::from_utf8_lossy(stderr).trim()
    )
}

fn command_output_detail(output: &std::process::Output) -> String {
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if detail.is_empty() {
        output.status.to_string()
    } else {
        detail
    }
}

fn command_output_streams(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = [stdout.trim(), stderr.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    if detail.is_empty() {
        output.status.to_string()
    } else {
        detail
    }
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
