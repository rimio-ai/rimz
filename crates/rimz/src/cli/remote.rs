//! `rimz remote` — named SSH room aliases and remote attach.

use std::io::{BufRead, BufReader, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::json;

use super::{
    AttachAction, AttachFlags, GlobalFlags, attach_action, exec_attach_command,
    workspace_record_for_session,
};
use rimz::ids::MuxName;
use rimz::remote::aliases::{RemoteAlias, RemoteAliases};
use rimz::remote::link::{
    LINK_BLACKOUT_AFTER, LinkAck, LinkProbe, LinkStatsFile, ProbeWindow, control_check_spec,
    probe_interval_from_env, probe_stream_spec, probe_timeout_from_env,
};
use rimz::remote::{RemoteTarget, ssh_attach_spec, ssh_attach_spec_with_control};

const CONTROL_MASTER_CHECK_INTERVAL: Duration = Duration::from_millis(50);
const CONTROL_MASTER_CHECK_TIMEOUT: Duration = Duration::from_millis(500);
const PROBE_STREAM_BLACKOUT_FAILURES: u32 = 3;
const PROBE_RESPAWN_BACKOFF_MIN: Duration = Duration::from_secs(1);
const PROBE_RESPAWN_BACKOFF_MAX: Duration = Duration::from_secs(30);
const LINK_SCHEMA_MISMATCH_EXIT: i32 = 2;

#[derive(Debug, Args)]
pub struct RemoteArgs {
    #[command(subcommand)]
    command: RemoteSubcmd,
}

#[derive(Debug, Subcommand)]
enum RemoteSubcmd {
    /// Save a named remote target.
    #[command(
        after_help = "With `remote add`, --mux <name> pins the saved alias when written under `remote` or `add`. A top-level `rimz --mux <name> remote add ...` is not saved."
    )]
    Add {
        name: String,
        target: String,
        /// Hand the link to a single ssh run instead of supervising reconnects.
        #[arg(long)]
        no_reconnect: bool,
        /// Come up empty when this alias births a remote room.
        #[arg(long)]
        no_resume: bool,
    },
    /// Connect to a remote alias or raw `[user@]host:<session-or-path>` target.
    Connect {
        alias_or_target: String,
        /// Force a fresh remote room by passing `--no-resume` to the remote rimz.
        #[arg(long)]
        reset: bool,
        /// Hand the link to a single ssh run instead of supervising reconnects.
        #[arg(long)]
        no_reconnect: bool,
        #[command(flatten)]
        attach: AttachFlags,
    },
    /// Connect to a remote alias or raw target with `--no-resume`.
    Reset {
        alias_or_target: String,
        /// Hand the link to a single ssh run instead of supervising reconnects.
        #[arg(long)]
        no_reconnect: bool,
        #[command(flatten)]
        attach: AttachFlags,
    },
    /// Delete a saved remote alias.
    #[command(name = "del", visible_alias = "rm")]
    Delete { name: String },
    /// Rename a saved remote alias.
    Rename { old: String, new: String },
    /// List saved remote aliases.
    #[clap(visible_alias = "ls")]
    List {
        #[arg(long)]
        json: bool,
    },
    /// Hidden remote-link stats plumbing. The SSH probe stream calls this.
    #[command(name = "link-stats", hide = true)]
    LinkStats {
        #[command(subcommand)]
        command: LinkStatsSubcmd,
    },
}

#[derive(Debug, Subcommand)]
enum LinkStatsSubcmd {
    /// Ingest JSONL link probes for one remote room and publish link-stats.json.
    Ingest(LinkStatsIngestArgs),
}

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
struct LinkStatsIngestArgs {
    /// Existing room session name.
    #[arg(long)]
    session: Option<String>,
    /// Room directory, resolved like `rimz start <dir>`.
    #[arg(long)]
    dir: Option<PathBuf>,
}

pub fn run(args: RemoteArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        RemoteSubcmd::Add {
            name,
            target,
            no_reconnect,
            no_resume,
        } => {
            let mut aliases = RemoteAliases::load().context("loading remote aliases")?;
            aliases.add(RemoteAlias {
                name,
                target,
                reconnect: !no_reconnect,
                no_resume,
                mux: add_persistent_mux(globals),
            })?;
            aliases.save().context("saving remote aliases")?;
            Ok(())
        }
        RemoteSubcmd::Connect {
            alias_or_target,
            reset,
            no_reconnect,
            attach,
        } => {
            let aliases = RemoteAliases::load().context("loading remote aliases")?;
            let remote =
                resolve_connect(&alias_or_target, reset, no_reconnect, globals.mux, &aliases)?;
            attach_remote(remote, attach.mode())
        }
        RemoteSubcmd::Reset {
            alias_or_target,
            no_reconnect,
            attach,
        } => {
            let aliases = RemoteAliases::load().context("loading remote aliases")?;
            let remote =
                resolve_connect(&alias_or_target, true, no_reconnect, globals.mux, &aliases)?;
            attach_remote(remote, attach.mode())
        }
        RemoteSubcmd::Delete { name } => {
            let mut aliases = RemoteAliases::load().context("loading remote aliases")?;
            aliases.remove(&name)?;
            aliases.save().context("saving remote aliases")?;
            Ok(())
        }
        RemoteSubcmd::Rename { old, new } => {
            let mut aliases = RemoteAliases::load().context("loading remote aliases")?;
            aliases.rename(&old, new)?;
            aliases.save().context("saving remote aliases")?;
            Ok(())
        }
        RemoteSubcmd::List { json } => {
            let aliases = RemoteAliases::load().context("loading remote aliases")?;
            print_list(aliases.entries(), json);
            Ok(())
        }
        RemoteSubcmd::LinkStats { command } => match command {
            LinkStatsSubcmd::Ingest(args) => ingest_link_stats(args),
        },
    }
}

fn add_persistent_mux(globals: &GlobalFlags) -> Option<MuxName> {
    remote_add_scopes_mux_flag(std::env::args_os())
        .then_some(globals.mux)
        .flatten()
}

fn remote_add_scopes_mux_flag<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut in_remote = false;
    let mut in_add = false;
    let mut mux_scoped_to_remote = false;

    for arg in args.into_iter().skip(1) {
        let Some(arg) = arg.as_ref().to_str() else {
            continue;
        };
        if !in_remote {
            in_remote = arg == "remote";
            continue;
        }
        if arg == "--" {
            break;
        }
        if is_mux_flag(arg) {
            if in_add {
                return true;
            }
            mux_scoped_to_remote = true;
            continue;
        }
        if !in_add && arg == "add" {
            in_add = true;
        }
    }

    in_add && mux_scoped_to_remote
}

fn is_mux_flag(arg: &str) -> bool {
    arg == "--mux" || arg.starts_with("--mux=")
}

#[derive(Debug)]
struct RemoteConnect {
    target: RemoteTarget,
    reconnect: bool,
    no_resume: bool,
    mux: Option<MuxName>,
}

fn resolve_connect(
    input: &str,
    reset: bool,
    no_reconnect: bool,
    cli_mux: Option<MuxName>,
    aliases: &RemoteAliases,
) -> Result<RemoteConnect> {
    if input.contains(':') {
        return Ok(RemoteConnect {
            target: RemoteTarget::parse(input)?,
            reconnect: !no_reconnect,
            no_resume: reset,
            mux: cli_mux,
        });
    }
    let Some(alias) = aliases.get(input) else {
        bail!("no such remote alias `{input}`; run `rimz remote list`");
    };
    Ok(RemoteConnect {
        target: RemoteTarget::parse(&alias.target)?,
        reconnect: alias.reconnect && !no_reconnect,
        no_resume: alias.no_resume || reset,
        mux: cli_mux.or(alias.mux),
    })
}

fn ingest_link_stats(args: LinkStatsIngestArgs) -> Result<()> {
    let (runtime, client) = link_stats_runtime(args)?;
    runtime
        .ensure_dirs()
        .context("preparing runtime directories for link stats")?;
    let path = rimz::remote::link::stats_path(&runtime);
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.context("reading link probe")?;
        if line.trim().is_empty() {
            continue;
        }
        let probe: LinkProbe = serde_json::from_str(&line).context("parsing link probe")?;
        if !probe.version_ok() {
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(stderr, "unsupported link probe schema `{}`", probe.v);
            std::process::exit(LINK_SCHEMA_MISMATCH_EXIT);
        }
        let file = LinkStatsFile::new(
            rimz::sidebar::cache::unix_now_ms(),
            client.clone(),
            probe.stats.clone(),
        );
        rimz::ledger::atomic::write_temp_then_rename_cache(&path, &file)
            .with_context(|| format!("writing {}", path.display()))?;
        serde_json::to_writer(&mut stdout, &LinkAck::new(probe.seq)).context("writing link ack")?;
        writeln!(stdout).context("writing link ack newline")?;
        stdout.flush().context("flushing link ack")?;
    }
    Ok(())
}

fn link_stats_runtime(args: LinkStatsIngestArgs) -> Result<(rimz::RuntimePaths, String)> {
    let workspace_id = match (args.session, args.dir) {
        (Some(session), None) => {
            workspace_record_for_session(&session)?
                .with_context(|| format!("no Rimz workspace record for session `{session}`"))?
                .workspace_id
        }
        (None, Some(dir)) => {
            rimz::WorkspaceResolver::resolve(&dir, None)
                .with_context(|| format!("resolving remote room dir {}", dir.display()))?
                .workspace_id
        }
        _ => bail!("give exactly one of --session or --dir"),
    };
    let runtime = rimz::RuntimePaths::for_workspace(workspace_id)?;
    Ok((runtime, link_client_id()))
}

fn link_client_id() -> String {
    std::env::var("SSH_CONNECTION").unwrap_or_else(|_| "ssh".to_owned())
}

/// SSH remote attach: the local rimz is a launcher and link supervisor only.
/// Workspace resolution, session birth, the sidebar, and the health gate all
/// run on the remote host's own `rimz`; the room renders here over `ssh -t`.
fn attach_remote(remote: RemoteConnect, mode: super::AttachMode) -> Result<()> {
    let plain_spec = ssh_attach_spec(&remote.target, remote.no_resume, remote.mux);

    // The local nesting block does not apply: a remote room inside a local
    // pane is a legitimate shape (the remote rimz checks its own env).
    match attach_action(
        mode,
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
        false,
    ) {
        AttachAction::Print => {
            print_remote_command(&plain_spec);
            Ok(())
        }
        AttachAction::Exec => {
            let program = rimz::remote::ssh_program();
            which::which(&program).map_err(|_| {
                anyhow::anyhow!(
                    "`{program}` is not on PATH; install an OpenSSH client to attach \
                     remotely, or run with --print to emit the command"
                )
            })?;
            if remote.reconnect {
                let control = rimz::remote::link::control_path();
                let control_spec = ssh_attach_spec_with_control(
                    &remote.target,
                    remote.no_resume,
                    remote.mux,
                    Some(&control),
                );
                supervise_remote(&control_spec, &plain_spec, &remote.target, &control)
            } else {
                report_remote_connect(remote.target.host_display(), false);
                exec_attach_command(&plain_spec)
            }
        }
    }
}

/// Run ssh and keep the link alive, autossh-style: a clean detach exits, a
/// dropped link on an established session reconnects with capped backoff, and
/// anything else fails with the remote's own error. The remote mux session
/// survives the drop by design, so reattaching is idempotent.
fn supervise_remote(
    control_spec: &rimz::mux::CommandSpec,
    plain_spec: &rimz::mux::CommandSpec,
    target: &RemoteTarget,
    control_path: &Path,
) -> Result<()> {
    use rimz::remote::{ReconnectPolicy, Verdict};

    let policy = ReconnectPolicy::from_env();
    let host = target.host_display();
    let mut established = false;
    let mut consecutive_failures: u32 = 0;
    let mut outage_active = false;
    report_remote_connect(host, true);
    loop {
        let control_ready = match prepare_control_path(control_path) {
            Ok(()) => true,
            Err(err) => {
                tracing::debug!(
                    path = %control_path.display(),
                    error = %err,
                    "ControlMaster unavailable; continuing without link probes"
                );
                false
            }
        };
        let (events_tx, events_rx) = mpsc::channel();
        let probe = if control_ready {
            spawn_probe_loop(target.clone(), control_path.to_path_buf(), events_tx)
        } else {
            drop(events_tx);
            ProbeHandle::disabled()
        };
        let spec = if control_ready {
            control_spec
        } else {
            plain_spec
        };
        let restore_existing_outage_after_gatetime = outage_active;
        let outcome = run_ssh_session(
            spec,
            host,
            &events_rx,
            &mut outage_active,
            policy.gatetime,
            restore_existing_outage_after_gatetime,
        )?;
        probe.stop();
        if control_ready {
            remove_control_path(control_path);
        }
        if outcome.duration >= policy.gatetime {
            established = true;
            consecutive_failures = 0;
        }
        match rimz::remote::verdict(
            outcome.status.code(),
            established,
            consecutive_failures,
            &policy,
        ) {
            Verdict::CleanExit => return Ok(()),
            Verdict::Fatal { code } => bail!(
                "ssh to {host} exited with status {code}; not reconnecting \
                 (only a dropped link on an established session is retried)"
            ),
            Verdict::Retry { delay } => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                let mut stderr = std::io::stderr().lock();
                let _ = writeln!(
                    stderr,
                    "rimz: link to {host} lost — reconnecting in {}s (attempt {consecutive_failures}); Ctrl-C stops",
                    delay.as_secs(),
                );
                drop(stderr);
                if !outage_active {
                    outage_active = true;
                    emit_local_link_notification(
                        rimz::sidebar::notify::NotificationKind::LinkLost,
                        "Rimz: remote link lost",
                        &format!("SSH to {host} dropped; reconnecting."),
                        LocalLinkNotificationDelivery::TerminalAndCommand,
                    );
                }
                std::thread::sleep(delay);
            }
        }
    }
}

fn run_ssh_session(
    spec: &rimz::mux::CommandSpec,
    host: &str,
    events: &mpsc::Receiver<LinkEvent>,
    outage_active: &mut bool,
    gatetime: Duration,
    restore_existing_outage_after_gatetime: bool,
) -> Result<SessionOutcome> {
    let mut child = spec
        .to_command()
        .spawn()
        .with_context(|| format!("running `{}`", rimz::remote::display_ssh_command(spec)))?;
    let started = Instant::now();
    let mut reported_established = false;
    loop {
        if let Some(status) = child.try_wait().context("polling ssh session")? {
            return Ok(SessionOutcome {
                status,
                duration: started.elapsed(),
            });
        }
        if !reported_established && started.elapsed() >= gatetime {
            reported_established = true;
            if should_report_gatetime_restored(
                restore_existing_outage_after_gatetime,
                *outage_active,
            ) {
                report_link_restored(host, outage_active);
            }
        }
        match recv_link_event(
            events,
            ssh_session_poll_interval(started, gatetime, reported_established),
        ) {
            Some(event) => {
                handle_link_event(host, event, outage_active);
                while let Ok(event) = events.try_recv() {
                    handle_link_event(host, event, outage_active);
                }
            }
            None => {}
        }
    }
}

fn should_report_gatetime_restored(
    restore_existing_outage_after_gatetime: bool,
    outage_active: bool,
) -> bool {
    restore_existing_outage_after_gatetime && outage_active
}

fn ssh_session_poll_interval(
    started: Instant,
    gatetime: Duration,
    reported_established: bool,
) -> Duration {
    let poll = Duration::from_millis(200);
    if reported_established {
        return poll;
    }
    let elapsed = started.elapsed();
    if elapsed >= gatetime {
        Duration::ZERO
    } else {
        poll.min(gatetime - elapsed)
    }
}

#[derive(Debug)]
struct SessionOutcome {
    status: std::process::ExitStatus,
    duration: Duration,
}

fn recv_link_event(events: &mpsc::Receiver<LinkEvent>, poll: Duration) -> Option<LinkEvent> {
    match events.recv_timeout(poll) {
        Ok(event) => Some(event),
        Err(mpsc::RecvTimeoutError::Timeout) => None,
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            std::thread::sleep(poll);
            None
        }
    }
}

#[derive(Debug)]
enum LinkEvent {
    FirstAck,
    Blackout(Duration),
    Recovered,
}

fn handle_link_event(host: &str, event: LinkEvent, outage_active: &mut bool) {
    match event {
        LinkEvent::FirstAck if *outage_active => report_link_restored(host, outage_active),
        LinkEvent::FirstAck => {}
        LinkEvent::Blackout(duration) => {
            if *outage_active {
                return;
            }
            *outage_active = true;
            emit_local_link_notification(
                rimz::sidebar::notify::NotificationKind::LinkLost,
                "Rimz: remote link stalled",
                &format!(
                    "No probe ack from {host} for {}s.",
                    duration.as_secs().max(1)
                ),
                LocalLinkNotificationDelivery::TerminalOnly,
            );
        }
        LinkEvent::Recovered => report_link_restored(host, outage_active),
    }
}

fn report_link_restored(host: &str, outage_active: &mut bool) {
    if !*outage_active {
        return;
    }
    *outage_active = false;
    emit_local_link_notification(
        rimz::sidebar::notify::NotificationKind::LinkRestored,
        "Rimz: remote link restored",
        &format!("SSH to {host} is responsive again."),
        LocalLinkNotificationDelivery::TerminalAndCommand,
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LocalLinkNotificationDelivery {
    TerminalOnly,
    TerminalAndCommand,
}

impl LocalLinkNotificationDelivery {
    fn allows_command(self) -> bool {
        matches!(self, Self::TerminalAndCommand)
    }
}

fn emit_local_link_notification(
    kind: rimz::sidebar::notify::NotificationKind,
    title: &str,
    body: &str,
    delivery: LocalLinkNotificationDelivery,
) {
    let prefs = rimz::config::MachineConfig::load()
        .map(|config| config.notifications)
        .unwrap_or_default();
    let bytes = local_link_terminal_notification_bytes(
        title,
        body,
        &prefs,
        std::io::stderr().is_terminal(),
    );
    if !bytes.is_empty() {
        let mut stderr = std::io::stderr().lock();
        let _ = stderr.write_all(&bytes);
        let _ = stderr.flush();
    }
    let Some(notification) = local_link_command_notification(kind, title, body, delivery, &prefs)
    else {
        return;
    };
    let Some(command) = prefs.command() else {
        return;
    };
    if let Err(err) = rimz::sidebar::notify::spawn_notify_command(command, &notification) {
        tracing::debug!(error = %err, "link notify-command spawn failed");
    }
}

fn local_link_terminal_notification_bytes(
    title: &str,
    body: &str,
    prefs: &rimz::config::NotificationsPrefs,
    stderr_is_terminal: bool,
) -> Vec<u8> {
    if stderr_is_terminal {
        rimz::osc::local_terminal_notification_bytes(prefs, title, body)
    } else {
        Vec::new()
    }
}

fn local_link_command_notification(
    kind: rimz::sidebar::notify::NotificationKind,
    title: &str,
    body: &str,
    delivery: LocalLinkNotificationDelivery,
    prefs: &rimz::config::NotificationsPrefs,
) -> Option<rimz::sidebar::notify::Notification> {
    if !prefs.enabled || !delivery.allows_command() || prefs.command().is_none() {
        return None;
    }
    Some(rimz::sidebar::notify::Notification {
        agents: Vec::new(),
        notification_kind: kind,
        title: title.to_owned(),
        body: body.to_owned(),
    })
}

fn prepare_control_path(path: &Path) -> Result<()> {
    let link_dir = path
        .parent()
        .with_context(|| format!("SSH control path {} has no parent", path.display()))?;
    let rimz_dir = link_dir
        .parent()
        .with_context(|| format!("SSH control directory {} has no parent", link_dir.display()))?;
    let runtime_dir = rimz_dir
        .parent()
        .with_context(|| format!("SSH control directory {} has no parent", rimz_dir.display()))?;
    ensure_private_control_dir(runtime_dir)?;
    ensure_private_control_dir(rimz_dir)?;
    ensure_private_control_dir(link_dir)?;
    remove_control_path(path);
    Ok(())
}

#[cfg(unix)]
fn ensure_private_control_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .with_context(|| format!("creating SSH control directory {}", path.display()))?;
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("checking SSH control directory {}", path.display()))?;
    if !metadata.is_dir() {
        bail!("SSH control path {} is not a directory", path.display());
    }
    let uid = nix::unistd::Uid::current().as_raw();
    if metadata.uid() != uid {
        bail!(
            "SSH control directory {} is owned by uid {}, not uid {uid}",
            path.display(),
            metadata.uid()
        );
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("hardening SSH control directory {}", path.display()))?;
        let metadata = std::fs::metadata(path)
            .with_context(|| format!("checking SSH control directory {}", path.display()))?;
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!(
                "SSH control directory {} is accessible by group or other users",
                path.display()
            );
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_private_control_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)
        .with_context(|| format!("creating SSH control directory {}", path.display()))
}

fn remove_control_path(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            tracing::debug!(path = %path.display(), error = %err, "remove SSH control socket failed")
        }
    }
}

struct ProbeHandle {
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl ProbeHandle {
    fn disabled() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(true)),
            join: None,
        }
    }

    fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn spawn_probe_loop(
    target: RemoteTarget,
    control_path: PathBuf,
    events: mpsc::Sender<LinkEvent>,
) -> ProbeHandle {
    let Some(interval) = probe_interval_from_env() else {
        return ProbeHandle::disabled();
    };
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let join = std::thread::spawn(move || {
        probe_loop(target, control_path, interval, events, thread_stop);
    });
    ProbeHandle {
        stop,
        join: Some(join),
    }
}

fn wait_for_control_master(target: &RemoteTarget, control_path: &Path, stop: &AtomicBool) -> bool {
    while !stop.load(Ordering::Relaxed) {
        if control_check_spec(target, control_path)
            .run_with_timeout(CONTROL_MASTER_CHECK_TIMEOUT)
            .is_ok()
        {
            return true;
        }
        sleep_interruptibly(CONTROL_MASTER_CHECK_INTERVAL, stop);
    }
    false
}

fn probe_loop(
    target: RemoteTarget,
    control_path: PathBuf,
    interval: Duration,
    events: mpsc::Sender<LinkEvent>,
    stop: Arc<AtomicBool>,
) {
    let mut window = ProbeWindow::with_timeout(probe_timeout_from_env());
    let mut seq: u64 = 0;
    let mut blackout_latched = false;
    let mut seen_ack = false;
    let mut consecutive_failures = 0u32;
    let mut respawn_backoff = ProbeRespawnBackoff::default();
    while !stop.load(Ordering::Relaxed) {
        if !wait_for_control_master(&target, &control_path, &stop) {
            return;
        }
        match run_probe_stream(
            &target,
            &control_path,
            interval,
            &events,
            &stop,
            &mut window,
            &mut seq,
            &mut blackout_latched,
            &mut seen_ack,
        ) {
            ProbeStreamExit::Stopped | ProbeStreamExit::VersionSkew => return,
            ProbeStreamExit::Ended { acked } => {
                let respawn_delay = if acked {
                    respawn_backoff.reset();
                    PROBE_RESPAWN_BACKOFF_MIN
                } else {
                    respawn_backoff.next_delay()
                };
                if acked {
                    consecutive_failures = 0;
                } else {
                    consecutive_failures = consecutive_failures.saturating_add(1);
                }
                if consecutive_failures >= PROBE_STREAM_BLACKOUT_FAILURES {
                    maybe_send_probe_blackout(
                        &events,
                        &mut window,
                        &mut blackout_latched,
                        seen_ack,
                    );
                }
                sleep_interruptibly(respawn_delay, &stop);
            }
        }
    }
}

#[derive(Debug, Default)]
struct ProbeRespawnBackoff {
    failures: u32,
}

impl ProbeRespawnBackoff {
    fn reset(&mut self) {
        self.failures = 0;
    }

    fn next_delay(&mut self) -> Duration {
        let shift = self.failures.min(5);
        self.failures = self.failures.saturating_add(1);
        PROBE_RESPAWN_BACKOFF_MIN
            .saturating_mul(1u32 << shift)
            .min(PROBE_RESPAWN_BACKOFF_MAX)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProbeStreamExit {
    Ended { acked: bool },
    Stopped,
    VersionSkew,
}

#[expect(
    clippy::too_many_arguments,
    reason = "probe loop state is explicit and testable"
)]
fn run_probe_stream(
    target: &RemoteTarget,
    control_path: &Path,
    interval: Duration,
    events: &mpsc::Sender<LinkEvent>,
    stop: &AtomicBool,
    window: &mut ProbeWindow,
    seq: &mut u64,
    blackout_latched: &mut bool,
    seen_ack: &mut bool,
) -> ProbeStreamExit {
    let mut child = match probe_stream_spec(target, control_path)
        .to_command()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            tracing::debug!(error = %err, "remote link probe stream spawn failed");
            return ProbeStreamExit::Ended { acked: false };
        }
    };
    let Some(stdout) = child.stdout.take() else {
        return ProbeStreamExit::Ended { acked: false };
    };
    let Some(mut stdin) = child.stdin.take() else {
        return ProbeStreamExit::Ended { acked: false };
    };
    let (ack_tx, ack_rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout)
            .lines()
            .map_while(std::result::Result::ok)
        {
            let Ok(ack) = serde_json::from_str::<LinkAck>(&line) else {
                continue;
            };
            if ack.version_ok() {
                let _ = ack_tx.send(ack.seq);
            }
        }
    });
    let mut next_tick = Instant::now();
    let mut acked = false;
    loop {
        if stop.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return ProbeStreamExit::Stopped;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let _ = reader.join();
                if matches!(
                    status.code(),
                    Some(rimz::remote::REMOTE_RIMZ_MISSING_EXIT | 2)
                ) {
                    return ProbeStreamExit::VersionSkew;
                }
                acked =
                    finish_probe_stream(&ack_rx, events, window, blackout_latched, seen_ack, acked);
                return ProbeStreamExit::Ended { acked };
            }
            Ok(None) => {}
            Err(err) => {
                tracing::debug!(error = %err, "remote link probe stream poll failed");
                let _ = reader.join();
                acked =
                    finish_probe_stream(&ack_rx, events, window, blackout_latched, seen_ack, acked);
                return ProbeStreamExit::Ended { acked };
            }
        }

        if drain_probe_acks(&ack_rx, events, window, blackout_latched, seen_ack) {
            acked = true;
        }
        maybe_send_probe_blackout(events, window, blackout_latched, *seen_ack);

        if Instant::now() >= next_tick {
            let sent_at_ms = rimz::sidebar::cache::unix_now_ms();
            let probe = LinkProbe::new(*seq, sent_at_ms, window.stats());
            window.record_sent(*seq, sent_at_ms);
            *seq = (*seq).saturating_add(1);
            if serde_json::to_writer(&mut stdin, &probe).is_err()
                || writeln!(stdin).is_err()
                || stdin.flush().is_err()
            {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                acked =
                    finish_probe_stream(&ack_rx, events, window, blackout_latched, seen_ack, acked);
                return ProbeStreamExit::Ended { acked };
            }
            next_tick = Instant::now() + interval;
        }
        sleep_until_next_tick(next_tick, stop);
    }
}

fn finish_probe_stream(
    ack_rx: &mpsc::Receiver<u64>,
    events: &mpsc::Sender<LinkEvent>,
    window: &mut ProbeWindow,
    blackout_latched: &mut bool,
    seen_ack: &mut bool,
    acked: bool,
) -> bool {
    drain_probe_acks(ack_rx, events, window, blackout_latched, seen_ack) || acked
}

fn maybe_send_probe_blackout(
    events: &mpsc::Sender<LinkEvent>,
    window: &mut ProbeWindow,
    blackout_latched: &mut bool,
    seen_ack: bool,
) {
    let now_ms = rimz::sidebar::cache::unix_now_ms();
    maybe_send_probe_blackout_at(events, window, blackout_latched, seen_ack, now_ms);
}

fn maybe_send_probe_blackout_at(
    events: &mpsc::Sender<LinkEvent>,
    window: &mut ProbeWindow,
    blackout_latched: &mut bool,
    seen_ack: bool,
    now_ms: u64,
) {
    if !seen_ack {
        return;
    }
    window.expire(now_ms);
    let blackout_ms = window.blackout_ms(now_ms);
    if blackout_ms >= LINK_BLACKOUT_AFTER.as_millis() as u64 && !*blackout_latched {
        *blackout_latched = true;
        let _ = events.send(LinkEvent::Blackout(Duration::from_millis(blackout_ms)));
    }
}

fn drain_probe_acks(
    ack_rx: &mpsc::Receiver<u64>,
    events: &mpsc::Sender<LinkEvent>,
    window: &mut ProbeWindow,
    blackout_latched: &mut bool,
    seen_ack: &mut bool,
) -> bool {
    let mut acked = false;
    while let Ok(seq) = ack_rx.try_recv() {
        let now_ms = rimz::sidebar::cache::unix_now_ms();
        if !window.record_ack(seq, now_ms) {
            continue;
        }
        acked = true;
        if !*seen_ack {
            *seen_ack = true;
            let _ = events.send(LinkEvent::FirstAck);
        }
        if *blackout_latched {
            *blackout_latched = false;
            let _ = events.send(LinkEvent::Recovered);
        }
    }
    acked
}

fn sleep_until_next_tick(next_tick: Instant, stop: &AtomicBool) {
    let now = Instant::now();
    let until_tick = next_tick.saturating_duration_since(now);
    sleep_interruptibly(until_tick.min(Duration::from_millis(50)), stop);
}

fn sleep_interruptibly(duration: Duration, stop: &AtomicBool) {
    if duration.is_zero() {
        return;
    }
    let step = Duration::from_millis(50);
    let deadline = Instant::now() + duration;
    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        std::thread::sleep((deadline - now).min(step));
    }
}

/// One stderr line before the terminal belongs to ssh, so the user knows the
/// room they are about to see is remote.
fn report_remote_connect(host: &str, reconnect: bool) {
    let mut stderr = std::io::stderr().lock();
    let tail = if reconnect {
        " (auto-reconnect on; Ctrl-C stops)"
    } else {
        ""
    };
    let _ = writeln!(stderr, "rimz: attaching to {host} over ssh…{tail}");
}

fn print_remote_command(spec: &rimz::mux::CommandSpec) {
    #[expect(clippy::print_stdout, reason = "user-facing command suggestion")]
    {
        println!("{}", rimz::remote::display_ssh_command(spec));
    }
}

#[derive(Serialize)]
struct ListEntryJson<'a> {
    name: &'a str,
    target: &'a str,
    reconnect: bool,
    no_resume: bool,
    mux: Option<&'a str>,
}

fn print_list(entries: &[RemoteAlias], json: bool) {
    if json {
        let rendered = render_list_json(entries);
        #[expect(clippy::print_stdout, reason = "json emitter")]
        {
            println!("{rendered}");
        }
        return;
    }
    let rendered = render_list_human(entries);
    if rendered.is_empty() {
        return;
    }
    #[expect(clippy::print_stdout, reason = "human listing")]
    {
        println!("{rendered}");
    }
}

fn render_list_json(entries: &[RemoteAlias]) -> String {
    let rows: Vec<ListEntryJson<'_>> = entries
        .iter()
        .map(|entry| ListEntryJson {
            name: &entry.name,
            target: &entry.target,
            reconnect: entry.reconnect,
            no_resume: entry.no_resume,
            mux: entry.mux.map(|mux| mux.as_str()),
        })
        .collect();
    serde_json::to_string_pretty(&json!({ "remotes": rows })).expect("rendered JSON serializes")
}

fn render_list_human(entries: &[RemoteAlias]) -> String {
    let mut buf = String::new();
    for entry in entries {
        let reconnect = if entry.reconnect {
            "reconnect"
        } else {
            "no-reconnect"
        };
        let no_resume = if entry.no_resume {
            "no-resume"
        } else {
            "resume"
        };
        let mux = entry
            .mux
            .map(|mux| mux.as_str().to_owned())
            .unwrap_or_else(|| "-".to_owned());
        use std::fmt::Write as _;
        writeln!(
            buf,
            "{}\t{}\t{}\t{}\t{}",
            entry.name, entry.target, reconnect, no_resume, mux,
        )
        .expect("write to string");
    }
    buf.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::time::{Duration, Instant};

    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    fn alias(name: &str, target: &str, reconnect: bool, no_resume: bool) -> RemoteAlias {
        RemoteAlias {
            name: name.to_owned(),
            target: target.to_owned(),
            reconnect,
            no_resume,
            mux: None,
        }
    }

    #[test]
    fn remote_add_scopes_mux_from_remote_or_add_position() {
        assert!(!remote_add_scopes_mux_flag(args(&[
            "rimz", "--mux", "tmux", "remote", "add", "name", "target",
        ])));
        assert!(remote_add_scopes_mux_flag(args(&[
            "rimz", "remote", "--mux", "tmux", "add", "name", "target",
        ])));
        assert!(remote_add_scopes_mux_flag(args(&[
            "rimz", "remote", "add", "--mux", "tmux", "name", "target",
        ])));
        assert!(remote_add_scopes_mux_flag(args(&[
            "rimz",
            "remote",
            "add",
            "name",
            "target",
            "--mux=tmux",
        ])));
        assert!(!remote_add_scopes_mux_flag(args(&[
            "rimz", "remote", "add", "name", "target", "--", "--mux", "tmux",
        ])));
    }

    #[test]
    fn connect_disambiguates_raw_targets_from_aliases() {
        let mut aliases = RemoteAliases::default();
        aliases
            .add(alias("prod", "prod-box:query-engine", true, false))
            .unwrap();

        let raw = resolve_connect("raw-box:session", false, false, None, &aliases).unwrap();
        let raw_spec = ssh_attach_spec(&raw.target, raw.no_resume, raw.mux);
        assert_eq!(raw_spec.args[8], "raw-box");

        let named = resolve_connect("prod", false, false, None, &aliases).unwrap();
        let named_spec = ssh_attach_spec(&named.target, named.no_resume, named.mux);
        assert_eq!(named_spec.args[8], "prod-box");
    }

    #[test]
    fn reset_and_alias_no_resume_force_no_resume() {
        let mut aliases = RemoteAliases::default();
        aliases
            .add(alias("fresh", "prod-box:query-engine", true, true))
            .unwrap();
        aliases
            .add(alias("default", "dev-box:query-engine", true, false))
            .unwrap();

        let fresh = resolve_connect("fresh", false, false, None, &aliases).unwrap();
        assert!(fresh.no_resume);

        let reset = resolve_connect("default", true, false, None, &aliases).unwrap();
        assert!(reset.no_resume);
    }

    #[test]
    fn no_reconnect_overrides_alias_default() {
        let mut aliases = RemoteAliases::default();
        aliases
            .add(alias("prod", "prod-box:query-engine", true, false))
            .unwrap();
        let remote = resolve_connect("prod", false, true, None, &aliases).unwrap();
        assert!(!remote.reconnect);
    }

    #[test]
    fn disconnected_link_event_channel_keeps_poll_cadence() {
        let (tx, rx) = std::sync::mpsc::channel::<LinkEvent>();
        drop(tx);
        let poll = Duration::from_millis(20);
        let started = Instant::now();

        assert!(recv_link_event(&rx, poll).is_none());

        assert!(
            started.elapsed() >= Duration::from_millis(15),
            "a disconnected probe channel must not hot-poll the ssh child"
        );
    }

    #[test]
    fn gatetime_restore_only_reports_preexisting_outage() {
        assert!(
            should_report_gatetime_restored(true, true),
            "a post-retry session passing gatetime reports recovery"
        );
        assert!(
            !should_report_gatetime_restored(false, true),
            "a blackout that began inside this session is not recovered by gatetime"
        );
        assert!(
            !should_report_gatetime_restored(true, false),
            "no active outage means no recovery edge"
        );
    }

    #[test]
    fn probe_respawn_backoff_is_capped_and_resettable() {
        let mut backoff = ProbeRespawnBackoff::default();

        assert_eq!(backoff.next_delay(), Duration::from_secs(1));
        assert_eq!(backoff.next_delay(), Duration::from_secs(2));
        assert_eq!(backoff.next_delay(), Duration::from_secs(4));
        assert_eq!(backoff.next_delay(), Duration::from_secs(8));
        assert_eq!(backoff.next_delay(), Duration::from_secs(16));
        assert_eq!(backoff.next_delay(), Duration::from_secs(30));
        assert_eq!(backoff.next_delay(), Duration::from_secs(30));

        backoff.reset();
        assert_eq!(backoff.next_delay(), Duration::from_secs(1));
    }

    #[test]
    fn finish_probe_stream_drains_tail_ack() {
        let (ack_tx, ack_rx) = std::sync::mpsc::channel::<u64>();
        let (event_tx, event_rx) = std::sync::mpsc::channel::<LinkEvent>();
        let mut window = ProbeWindow::with_timeout(Duration::from_millis(100));
        let mut blackout_latched = false;
        let mut seen_ack = false;
        let sent_at_ms = rimz::sidebar::cache::unix_now_ms().saturating_sub(10);

        window.record_sent(7, sent_at_ms);
        ack_tx.send(7).expect("send tail ack");
        drop(ack_tx);

        assert!(finish_probe_stream(
            &ack_rx,
            &event_tx,
            &mut window,
            &mut blackout_latched,
            &mut seen_ack,
            false,
        ));
        assert!(seen_ack);
        assert!(!blackout_latched);
        match event_rx.try_recv().expect("first ack event") {
            LinkEvent::FirstAck => {}
            other => panic!("expected first ack event, got {other:?}"),
        }
        let stats = window.stats();
        assert_eq!(stats.window, 1);
        assert_eq!(stats.miss_pct, 0);
        assert!(stats.rtt_ms.is_some());
    }

    #[test]
    fn ended_probe_stream_waits_for_blackout_threshold() {
        let (tx, rx) = std::sync::mpsc::channel::<LinkEvent>();
        let mut window = ProbeWindow::with_timeout(Duration::from_millis(100));
        let mut blackout_latched = false;
        let seen_ack = true;
        let blackout_after_ms = LINK_BLACKOUT_AFTER.as_millis() as u64;

        window.record_sent(1, 1_000);
        assert!(window.record_ack(1, 1_020));

        maybe_send_probe_blackout_at(
            &tx,
            &mut window,
            &mut blackout_latched,
            seen_ack,
            1_020 + blackout_after_ms - 1,
        );
        assert!(rx.try_recv().is_err());
        assert!(!blackout_latched);

        maybe_send_probe_blackout_at(
            &tx,
            &mut window,
            &mut blackout_latched,
            seen_ack,
            1_020 + blackout_after_ms,
        );
        match rx.try_recv().expect("blackout event") {
            LinkEvent::Blackout(duration) => assert_eq!(duration, LINK_BLACKOUT_AFTER),
            other => panic!("expected blackout event, got {other:?}"),
        }
        assert!(blackout_latched);

        maybe_send_probe_blackout_at(
            &tx,
            &mut window,
            &mut blackout_latched,
            seen_ack,
            1_020 + blackout_after_ms + 1_000,
        );
        assert!(
            rx.try_recv().is_err(),
            "latched blackout events are not repeated"
        );
    }

    #[test]
    fn probe_blackout_requires_prior_ack() {
        let (tx, rx) = std::sync::mpsc::channel::<LinkEvent>();
        let mut window = ProbeWindow::with_timeout(Duration::from_millis(100));
        let mut blackout_latched = false;
        let blackout_after_ms = LINK_BLACKOUT_AFTER.as_millis() as u64;

        window.record_sent(1, 1_000);
        maybe_send_probe_blackout_at(
            &tx,
            &mut window,
            &mut blackout_latched,
            false,
            1_000 + blackout_after_ms,
        );

        assert!(rx.try_recv().is_err());
        assert!(!blackout_latched);
    }

    #[test]
    fn blackout_delivery_suppresses_notify_command() {
        let prefs = rimz::config::NotificationsPrefs {
            enabled: true,
            command: Some("notify-send rimz".to_owned()),
            ..rimz::config::NotificationsPrefs::default()
        };

        assert!(
            local_link_command_notification(
                rimz::sidebar::notify::NotificationKind::LinkLost,
                "Rimz: remote link stalled",
                "No probe ack from dev for 8s.",
                LocalLinkNotificationDelivery::TerminalOnly,
                &prefs,
            )
            .is_none(),
            "blackout delivery is terminal-only"
        );

        let notification = local_link_command_notification(
            rimz::sidebar::notify::NotificationKind::LinkLost,
            "Rimz: remote link lost",
            "SSH to dev dropped; reconnecting.",
            LocalLinkNotificationDelivery::TerminalAndCommand,
            &prefs,
        )
        .expect("lost-link delivery spawns the configured command");
        assert_eq!(notification.kind_env(), "link_lost");
    }

    #[test]
    fn redirected_stderr_suppresses_terminal_notification_bytes() {
        let prefs = rimz::config::NotificationsPrefs::default();

        assert!(
            local_link_terminal_notification_bytes("Title", "Body", &prefs, false).is_empty(),
            "redirected stderr must not collect OSC or BEL bytes"
        );
        assert!(
            !local_link_terminal_notification_bytes("Title", "Body", &prefs, true).is_empty(),
            "terminal stderr keeps the configured terminal notification bytes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn prepare_control_path_hardens_control_directories() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = dir.path().join("runtime");
        let rimz_dir = runtime.join("rimz");
        let link_dir = rimz_dir.join("link");
        std::fs::create_dir_all(&link_dir).expect("mkdir link dir");
        for path in [&runtime, &rimz_dir, &link_dir] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o777))
                .expect("make dir world-accessible");
        }
        let control = link_dir.join("link.sock");

        prepare_control_path(&control).expect("prepare control path");

        for path in [&runtime, &rimz_dir, &link_dir] {
            let mode = std::fs::metadata(path)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "{} is private", path.display());
        }
    }

    #[test]
    fn list_json_emits_canonical_shape() {
        let entries = vec![
            RemoteAlias {
                name: "dev".to_owned(),
                target: "dev-box:query-engine".to_owned(),
                reconnect: true,
                no_resume: false,
                mux: None,
            },
            RemoteAlias {
                name: "prod".to_owned(),
                target: "agent@prod-box:~/code/query-engine".to_owned(),
                reconnect: false,
                no_resume: true,
                mux: Some(MuxName::Tmux),
            },
        ];
        insta::assert_snapshot!(render_list_json(&entries), @r#"
        {
          "remotes": [
            {
              "mux": null,
              "name": "dev",
              "no_resume": false,
              "reconnect": true,
              "target": "dev-box:query-engine"
            },
            {
              "mux": "tmux",
              "name": "prod",
              "no_resume": true,
              "reconnect": false,
              "target": "agent@prod-box:~/code/query-engine"
            }
          ]
        }
        "#);
    }

    #[test]
    fn list_human_emits_tab_separated_rows() {
        let entries = vec![RemoteAlias {
            name: "prod".to_owned(),
            target: "prod-box:query-engine".to_owned(),
            reconnect: true,
            no_resume: false,
            mux: Some(MuxName::Zellij),
        }];
        insta::assert_snapshot!(
            render_list_human(&entries),
            @"prod	prod-box:query-engine	reconnect	resume	zellij"
        );
    }
}
