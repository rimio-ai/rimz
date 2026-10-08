//! Hook subcommands. Installed hooks `exec` into these — stdout is the
//! agent decision channel; stderr is for diagnostics. The CLI marks the
//! whole subtree `hide = true` because users don't run it by hand.
//!
//! Ask hooks queue their payload and return the agent-native neutral no-op.
//! The drainer records `Waiting` when the agent has its own prompt surface;
//! the agent's UI stays the answer surface.

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use serde_json::Value;
use tracing::{debug, warn};

use super::{GlobalFlags, open_existing_store};
use rimz::Store;
use rimz::agents::lifecycle::LifecycleSignal;
use rimz::agents::{
    AgentDefinition, AgentHookClass, AgentLifecycleObservation, HookIngressAcceptance,
    HookIngressDecision, HookOutput, definition_by_kind,
};
use rimz::disk::lock::{IngressAppendLock, WorkspaceLock};
use rimz::harness::hook_drain;
use rimz::ids::{EventId, MuxName, PaneId};
use rimz::store::ingress::{self, HookIngress};
use rimz::store::writer::AgentLifecycleIntent;
use rimz::workspace::{self, ResolvedWorkspace, WorkspaceResolver};

mod binding;
mod hook_install;
mod install;
mod lifecycle;
mod owner;
mod proctree;

#[cfg(test)]
mod tests;

use binding::{enrich_pane_stamp_from_cache, recover_focused_pane_binding};
pub(in crate::cli) use hook_install::{ensure_detected_agent_hooks, install_hooks_into};
pub(crate) use install::{provider_home_logins, uninstall_managed_hooks};
use install::{run_install, run_uninstall};
use owner::{attach_agent_owner, attach_agent_pane, hook_agent_pid};
use proctree::sibling_agent_pins;
pub(super) const FOCUSED_PANE_BIND_TIMEOUT: std::time::Duration =
    std::time::Duration::from_millis(1_000);
const REPLY_WAIT: std::time::Duration = std::time::Duration::from_millis(1_200);
const APPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
#[derive(Debug, Args)]
pub struct HooksArgs {
    #[command(subcommand)]
    command: HooksSubcmd,
}

#[derive(Debug, Subcommand)]
enum HooksSubcmd {
    /// Receive a hook payload on stdin and route it through the agent
    /// adapter. Prints the agent-native stdout payload.
    #[command(hide = true)]
    Feed {
        #[arg(long)]
        source: String,
        /// Optional explicit event name. If absent, parsed from the payload.
        #[arg(long)]
        event: Option<String>,
    },
    #[command(hide = true)]
    Drain {
        #[arg(long)]
        project_root: PathBuf,
        #[arg(long)]
        once: bool,
    },
    #[command(hide = true)]
    Apply {
        #[arg(long)]
        recover_from: Option<String>,
        #[arg(long)]
        with_lifetime_lock: bool,
    },
    /// Install the adapter's hooks into the agent's per-user config file.
    /// Pass --dry-run to preview the config diff without writing files.
    /// Visible top-level command (not hidden) — the help text doubles as the
    /// install instruction.
    Install {
        /// Preview the hook config diff without writing files.
        #[arg(long)]
        dry_run: bool,
        /// Agent kind. Omit to install every detected agent.
        agent: Option<String>,
    },
    /// Remove the adapter's RimZ-managed hook block.
    Uninstall {
        /// Agent kind. Omit to remove every RimZ-managed hook set.
        agent: Option<String>,
    },
}

impl HooksArgs {
    /// The low-cardinality command label and the agent it acts on — the hook
    /// `source` for `feed`, the named agent for install/uninstall — for the
    /// Sentry command scope.
    pub(crate) fn scope(&self) -> (&'static str, Option<&str>) {
        match &self.command {
            HooksSubcmd::Feed { source, .. } => ("hooks feed", Some(source.as_str())),
            HooksSubcmd::Drain { .. } => ("hooks drain", None),
            HooksSubcmd::Apply { .. } => ("hooks apply", None),
            HooksSubcmd::Install { agent, .. } => ("hooks install", agent.as_deref()),
            HooksSubcmd::Uninstall { agent } => ("hooks uninstall", agent.as_deref()),
        }
    }
}

pub fn run(args: HooksArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        HooksSubcmd::Feed { source, event } => run_feed(source, event, globals),
        HooksSubcmd::Drain { project_root, once } => run_drain(project_root, once),
        HooksSubcmd::Apply {
            recover_from,
            with_lifetime_lock,
        } => run_apply(recover_from, with_lifetime_lock),
        HooksSubcmd::Install { agent, dry_run } => run_install(agent, dry_run),
        HooksSubcmd::Uninstall { agent } => run_uninstall(agent),
    }
}

pub(super) fn register_drainer() {
    rimz::harness::hook_drain::register_processor(process_in_child);
}

#[cfg(test)]
pub(super) fn register_test_drainer() {
    hook_drain::register_processor(|store, frame, _, _, _| {
        hook_drain::with_frame_env(frame, hook_drain::FrameAttempt::First, || {
            process_frame(
                &store.for_hook_ingress(frame.event_id.clone(), frame.ts),
                frame,
                &[],
            )
        })
    });
}

fn process_in_child(
    store: &Store,
    frame: &HookIngress,
    recover_from: Option<rimz::store::event_log::LogExtent>,
    lifetime: &WorkspaceLock,
    deadline: std::time::Instant,
) -> Result<rimz::agents::HookReply> {
    use std::io::Write as _;
    use std::os::fd::AsFd;
    use std::process::{Command, Stdio};

    let mut frame = frame.clone();
    map_sandbox_paths(store, &mut frame)?;
    if let Err(error) = std::fs::metadata(&frame.cwd) {
        if error.kind() != io::ErrorKind::NotFound {
            return Err(error.into());
        }
        frame.cwd = PathBuf::from(
            frame
                .env
                .get(workspace::ENV_PROJECT_ROOT)
                .context("hook apply requires a project root for a removed cwd")?,
        );
    }
    let binary = rimz::proc::rimz_exe();
    let mut command = Command::new(if binary.is_file() {
        binary
    } else {
        std::env::current_exe()?
    });
    command.args(["hooks", "apply", "--with-lifetime-lock"]);
    if let Some(extent) = recover_from {
        command
            .arg("--recover-from")
            .arg(serde_json::to_string(&extent)?);
    }
    let (mut sender, input) = std::os::unix::net::UnixStream::pair()?;
    let timeout =
        apply_timeout().min(deadline.saturating_duration_since(std::time::Instant::now()));
    anyhow::ensure!(!timeout.is_zero(), "hook drain deadline reached");
    sender.set_write_timeout(Some(timeout))?;
    let mut bytes = serde_json::to_vec(&frame)?;
    bytes.push(b'\n');
    let log_path = store.paths().hook_drainer_log();
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    command
        .env_clear()
        .envs(&frame.env)
        .current_dir(&frame.cwd)
        .stdin(Stdio::from(std::os::fd::OwnedFd::from(input)))
        .stdout(Stdio::piped())
        .stderr(log);
    #[cfg(feature = "testkit")]
    if let Some(socket) = std::env::var_os("RIMZ_TEST_HOOK_DRAIN_AFTER_LOCKED_APPLY") {
        command.env("RIMZ_TEST_HOOK_DRAIN_AFTER_LOCKED_APPLY", socket);
    }
    let deadline = deadline.min(std::time::Instant::now() + timeout);
    let child = command.spawn().context("spawning hook apply child")?;
    let sent = (|| -> Result<()> {
        let fds = [lifetime.as_fd()];
        let mut space = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut control = rustix::net::SendAncillaryBuffer::new(&mut space);
        anyhow::ensure!(
            control.push(rustix::net::SendAncillaryMessage::ScmRights(&fds)),
            "apply lock control buffer"
        );
        anyhow::ensure!(
            rustix::net::sendmsg(
                &sender,
                &[io::IoSlice::new(&bytes[..1])],
                &mut control,
                rustix::net::SendFlags::empty()
            )? == 1,
            "sending apply lifetime lock"
        );
        sender.write_all(&bytes[1..])?;
        Ok(())
    })();
    drop(sender);
    let output = hook_drain::wait_for_apply(child, deadline)?;
    sent?;
    anyhow::ensure!(
        output.status.success(),
        "hook apply child exited {}",
        output.status
    );
    if output.stdout.is_empty() {
        return Ok(rimz::agents::HookReply::Silent);
    }
    Ok(rimz::agents::HookReply::Json(serde_json::from_slice(
        &output.stdout,
    )?))
}

fn apply_timeout() -> std::time::Duration {
    #[cfg(feature = "testkit")]
    if let Some(ms) = std::env::var("RIMZ_TEST_HOOK_APPLY_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.parse().ok())
    {
        return std::time::Duration::from_millis(ms);
    }
    APPLY_TIMEOUT
}

fn map_sandbox_paths(store: &Store, frame: &mut HookIngress) -> Result<()> {
    if frame.env.get("RIMZ_ISOLATION").map(String::as_str) != Some("sandbox") {
        return Ok(());
    }
    let view = rimz::sandbox::TmpView::current(
        rimz::config::Isolation::Sandbox,
        frame.env.get("RIMZ_AGENT_NAME").map(String::as_str),
        store.paths(),
    );
    let rebound: Vec<PathBuf> = serde_json::from_str(
        frame
            .env
            .get(rimz::sandbox::HOOK_HOST_PATHS_ENV)
            .context("sandbox hook requires RIMZ_SANDBOX_HOST_PATHS")?,
    )?;
    let host_path = |path: &Path| {
        if rebound.iter().any(|root| path.starts_with(root)) {
            path.to_path_buf()
        } else {
            view.host_path(path)
        }
    };
    frame.cwd = host_path(&frame.cwd);
    for (key, value) in &mut frame.env {
        if key == "TMUX" {
            continue;
        }
        if key == "PATH" {
            *value =
                std::env::join_paths(std::env::split_paths(value).map(|path| host_path(&path)))?
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("captured PATH is not UTF-8"))?;
        } else if matches!(key.as_str(), "CLAUDE_CONFIG_DIR" | "PI_AGENT_DIR") {
            *value = value
                .split(',')
                .map(str::trim)
                .filter(|path| !path.is_empty())
                .map(|path| host_path(Path::new(path)).display().to_string())
                .collect::<Vec<_>>()
                .join(",");
        } else if Path::new(value).is_absolute() {
            *value = host_path(Path::new(value)).display().to_string();
        }
    }
    let mut payload: Value = if frame.payload.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&frame.payload)?
    };
    if let Some(fields) = payload.as_object_mut() {
        for key in [
            "cwd",
            "worktree_path",
            "transcript_path",
            "agent_transcript_path",
            "transcriptPath",
            "workspace_paths",
            "workspacePaths",
        ] {
            if let Some(value) = fields.get_mut(key) {
                let values = match value {
                    Value::Array(values) => values.as_mut_slice(),
                    value => std::slice::from_mut(value),
                };
                for value in values {
                    if let Some(path) = value.as_str().filter(|path| Path::new(path).is_absolute())
                    {
                        *value = Value::String(host_path(Path::new(path)).display().to_string());
                    }
                }
            }
        }
    }
    frame.payload = serde_json::to_string(&payload)?;
    Ok(())
}

fn run_apply(recover_from: Option<String>, with_lifetime_lock: bool) -> Result<()> {
    let mut prefix = Vec::new();
    let _lifetime = if with_lifetime_lock {
        let mut byte = [0];
        let mut space = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut control = rustix::net::RecvAncillaryBuffer::new(&mut space);
        let received = rustix::net::recvmsg(
            io::stdin(),
            &mut [io::IoSliceMut::new(&mut byte)],
            &mut control,
            rustix::net::RecvFlags::empty(),
        )?;
        anyhow::ensure!(received.bytes == 1, "missing apply lifetime lock");
        let lock = control
            .drain()
            .find_map(|message| match message {
                rustix::net::RecvAncillaryMessage::ScmRights(mut fds) => fds.next(),
                _ => None,
            })
            .context("missing apply lifetime lock descriptor")?;
        rustix::io::fcntl_setfd(&lock, rustix::io::FdFlags::CLOEXEC)?;
        prefix.extend(byte);
        Some(lock)
    } else {
        None
    };
    let frame: HookIngress =
        serde_json::from_reader(io::Cursor::new(prefix).chain(io::stdin().lock()))?;
    let root = frame
        .env
        .get(workspace::ENV_WORKSPACE_ID)
        .zip(frame.env.get(workspace::ENV_PROJECT_ROOT))
        .and_then(|(id, root)| workspace::verify_pin(id, Path::new(root)))
        .context("hook apply requires verified workspace pins")?;
    let paths = rimz::StatePaths::for_project_root(&root)?;
    let runtime = rimz::RuntimePaths::for_state(&paths)?;
    let store =
        Store::open_existing(paths, runtime).context("hook apply requires an existing room")?;
    let mut recovered = Vec::new();
    let redo = recover_from.is_some();
    if let Some(extent) = recover_from {
        let extent = serde_json::from_str(&extent)?;
        let mut tail = rimz::store::follow::LaunchTail::from_cursor(store.paths().clone(), extent);
        for warning in tail.poll(|event| {
            if event.ingress.as_ref() == Some(&frame.event_id) {
                recovered.push(event);
            }
        })? {
            warn!(%warning, "hook drain recovery");
        }
    }
    let replay = recovered.iter().any(|event| {
        matches!(
            event.kind(),
            rimz::store::event::EventKind::AgentLifecycle(_)
        )
    });
    let attempt = match (redo, replay) {
        (_, true) => hook_drain::FrameAttempt::Replay,
        (true, false) => hook_drain::FrameAttempt::Redo,
        (false, false) => hook_drain::FrameAttempt::First,
    };
    let reply = hook_drain::with_frame_env(&frame, attempt, || {
        process_frame(
            &store.for_hook_ingress(frame.event_id.clone(), frame.ts),
            &frame,
            if replay { &recovered } else { &[] },
        )
    })?;
    emit_reply(&reply)
}

fn run_drain(project_root: PathBuf, once: bool) -> Result<()> {
    let workspace =
        WorkspaceResolver::resolve_participant(&project_root, Some(project_root.clone()))?;
    let store =
        open_existing_store(&workspace)?.context("hook drainer requires an existing room")?;
    if once {
        rimz::harness::hook_drain::drain_through(
            &store,
            0,
            None,
            std::time::Duration::from_secs(30),
        )?;
    } else {
        rimz::harness::hook_drain::run(&store)?;
    }
    Ok(())
}

fn process_frame(
    store: &Store,
    frame: &rimz::store::ingress::HookIngress,
    recovered: &[rimz::store::event::EventEnvelope],
) -> Result<rimz::agents::HookReply> {
    let agent = definition_by_kind(frame.source.as_str())?;
    let owner = rimz::agents::HookIngressOwner {
        pid: frame
            .env
            .get("RIMZ_AGENT_PID")
            .and_then(|pid| pid.parse().ok()),
        kind: serde_json::from_value(Value::String(
            frame
                .env
                .get("RIMZ_HOOK_OWNER_KIND")
                .cloned()
                .unwrap_or_else(|| "agent".to_owned()),
        ))?,
    };
    let workspace = WorkspaceResolver::resolve_hook(
        &frame.cwd,
        // The frontend has already rejected ambient daemon pins and selected this room.
        false,
        &frame.env,
        &|cwd| sibling_agent_pins(frame.source.as_str(), cwd),
    )?;
    anyhow::ensure!(
        workspace.workspace_id == store.paths().workspace_id,
        "hook frame resolved outside its ingress room"
    );
    let payload: Value = if frame.payload.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&frame.payload)?
    };
    let event_name = frame
        .event
        .as_deref()
        .or_else(|| {
            payload
                .get("hook_event_name")
                .or_else(|| payload.get("hookEventName"))
                .and_then(Value::as_str)
        })
        .unwrap_or("unknown");
    let mut decoded = agent.decode_hook(event_name, &payload)?;
    if decoded.class() == AgentHookClass::AwaitingUser && !agent.spec().capabilities.native_ask_ui {
        return Ok(decoded.reply().clone());
    }
    let globals = GlobalFlags {
        mux: workspace.mux_hint,
        zellij: false,
        tmux: false,
        root: None,
        color: super::ColorWhen::Auto,
    };
    lifecycle::handle_lifecycle_frame(
        &workspace,
        store,
        agent,
        &mut decoded,
        &payload,
        owner,
        &globals,
        recovered,
    )
}

fn run_feed(source: String, event: Option<String>, globals: &GlobalFlags) -> Result<()> {
    let raw_agent_pid = hook_agent_pid(&source);
    let adapter = definition_by_kind(&source);
    let ingress = adapter
        .as_ref()
        .ok()
        .map(|adapter| adapter.hook_ingress(raw_agent_pid));
    if let Some(HookIngressDecision::Ignore(reason)) = ingress.as_ref() {
        debug!(
            source = %source,
            reason = reason.as_str(),
            "hooks feed: suppressed by adapter ingress policy",
        );
        return Ok(());
    }

    let agent = match adapter {
        Ok(agent) => agent,
        Err(err)
            if rimz::agents::plugins::loaded()
                .errors
                .iter()
                .any(|load_error| load_error.kind_hint.as_deref() == Some(source.as_str())) =>
        {
            warn!(source, error = %err, "hooks feed: invalid agent plugin skipped");
            return Ok(());
        }
        Err(err) => return Err(err.into()),
    };
    let acceptance = match ingress {
        Some(HookIngressDecision::Accept(acceptance)) => acceptance,
        Some(HookIngressDecision::Ignore(_)) => return Ok(()),
        None => HookIngressAcceptance::agent(raw_agent_pid),
    };
    let ingress_owner = acceptance.owner;
    let participant_start = acceptance
        .participant_start
        .unwrap_or_else(|| PathBuf::from("."));
    let scan = |cwd: &Path| sibling_agent_pins(&source, cwd);
    // A daemon's environment is unattributable: it can carry a valid workspace
    // pin for the unrelated room that launched the shared daemon. Daemon-owned
    // hooks never consult it. Pane-owned hooks keep the env pin first and use
    // sibling recovery when a daemon route cannot be classified.
    let mut env = hook_drain::capture_ingress_env(ingress_owner);
    let pinned_root = (globals.root.is_none()
        && ingress_owner.kind != rimz::RuntimeOwnerKind::Daemon)
        .then(|| {
            workspace::verify_pin(
                env.get(workspace::ENV_WORKSPACE_ID)?,
                Path::new(env.get(workspace::ENV_PROJECT_ROOT)?),
            )
        })
        .flatten();
    let project_root = if let Some(root) = pinned_root {
        root
    } else if ingress_owner.kind == rimz::RuntimeOwnerKind::Daemon {
        WorkspaceResolver::resolve_daemon_participant_with_pin_recovery(
            &participant_start,
            globals.root.clone(),
            &scan,
        )?
        .project_root
    } else {
        WorkspaceResolver::resolve_participant_with_pin_recovery(
            &participant_start,
            globals.root.clone(),
            &scan,
        )?
        .project_root
    };
    let mut buf = String::new();
    io::stdin()
        .read_to_string(&mut buf)
        .context("reading hook stdin")?;
    let payload: Value = if buf.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&buf).context("parsing hook payload")?
    };
    let event_name = event
        .or_else(|| {
            payload
                .get("hook_event_name")
                .or_else(|| payload.get("hookEventName"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_owned());
    let mut reply = agent.payload_hook_reply(&event_name, &payload);
    let paths = rimz::StatePaths::for_project_root(&project_root)?;
    let runtime = rimz::RuntimePaths::for_state(&paths)?;
    let Some(store) = Store::open_existing(paths, runtime) else {
        warn!(
            "rimz hooks feed: no room at {}; ignoring {} (run `rimz start` there)",
            project_root.display(),
            event_name,
        );
        return emit_reply(&reply);
    };
    env.extend(workspace::pin_env(
        &store.paths().workspace_id,
        &project_root,
    ));
    let frame = HookIngress {
        schema_version: "1".to_owned(),
        event_id: EventId::new(),
        ts: jiff::Timestamp::now(),
        source: agent.spec().kind_id(),
        event: Some(event_name.clone()),
        payload: buf,
        cwd: match participant_start.canonicalize() {
            Ok(cwd) => cwd,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let lexical = if participant_start.is_absolute() {
                    participant_start.clone()
                } else {
                    std::env::current_dir()
                        .ok()
                        .or_else(|| {
                            std::env::var_os("PWD")
                                .map(PathBuf::from)
                                .filter(|cwd| cwd.is_absolute())
                        })
                        .unwrap_or_else(|| project_root.clone())
                        .join(&participant_start)
                };
                rimz::utils::path::normalize_path_lexical(&lexical)
            }
            Err(error) => return Err(error.into()),
        },
        hook_pid: std::process::id(),
        env,
    };
    let sandbox_connection = if frame.env.get("RIMZ_ISOLATION").map(String::as_str)
        == Some("sandbox")
    {
        match frame
            .env
            .contains_key(rimz::sandbox::HOOK_HOST_PATHS_ENV)
            .then(|| hook_drain::Connection::connect(&store))
        {
            Some(Ok(connection)) => Some(connection),
            connection => {
                if let Some(Err(error)) = connection {
                    debug!(%error, "hooks feed: host drainer unavailable; applying in caller view");
                }
                let inline =
                    hook_drain::with_frame_env(&frame, hook_drain::FrameAttempt::First, || {
                        process_frame(&store, &frame, &[])
                    })?;
                if inline != rimz::agents::HookReply::Silent {
                    reply = inline;
                }
                if matches!(event_name.as_str(), "PostToolUse" | "postToolUse") {
                    let rung = attach_deadline_context(&store, agent, &event_name, &payload);
                    if rung != rimz::agents::HookReply::Silent {
                        reply = rung;
                    }
                }
                return emit_reply(&reply);
            }
        }
    } else {
        None
    };
    let through = {
        let lock = IngressAppendLock::acquire(&store.paths().hook_ingress_lock)?;
        ingress::append(store.paths(), &frame, &lock)?
    };
    if source == "antigravity"
        || (matches!(source.as_str(), "claude" | "codex") && event_name == "UserPromptSubmit")
    {
        let receipt = match sandbox_connection {
            Some(connection) => connection.reply(through, frame.event_id, REPLY_WAIT),
            None => hook_drain::drain_through(&store, through, Some(frame.event_id), REPLY_WAIT),
        };
        match receipt {
            Ok(receipt) => {
                if let Some(payload) = receipt.reply {
                    reply = rimz::agents::HookReply::Json(payload);
                }
            }
            Err(error) => debug!(%error, "hooks feed: prompt context was late"),
        }
        return emit_reply(&reply);
    }
    if matches!(event_name.as_str(), "PostToolUse" | "postToolUse") {
        let rung = attach_deadline_context(&store, agent, &event_name, &payload);
        if rung != rimz::agents::HookReply::Silent {
            reply = rung;
        }
    }
    if let Some(connection) = sandbox_connection {
        if let Err(error) = connection.nudge(through) {
            debug!(%error, "hooks feed: host drainer nudge failed");
        }
        return emit_reply(&reply);
    }
    if let Err(error) = hook_drain::nudge(&store, through) {
        warn!(%error, "hooks feed: drainer unavailable; draining inline");
        if let Err(error) = hook_drain::drain_through(&store, through, None, REPLY_WAIT) {
            warn!(%error, "hooks feed: inline drain failed");
        }
    }
    emit_reply(&reply)
}

fn attach_deadline_context(
    store: &Store,
    agent: &AgentDefinition,
    event: &str,
    payload: &Value,
) -> rimz::agents::HookReply {
    if agent.spec().capabilities.hook_context.is_none()
        || payload
            .get("agent_id")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty())
    {
        return rimz::agents::HookReply::Silent;
    }
    let Some(run_id) =
        hook_drain::env_value("RIMZ_RUN_ID").and_then(|id| rimz::RunId::parse(&id).ok())
    else {
        return rimz::agents::HookReply::Silent;
    };
    let mut decoded = HookOutput::new(rimz::agents::ClassifiedHook {
        class: AgentHookClass::Unknown,
        ask_kind: None,
        event_name: event.to_owned(),
    });
    if let Err(error) = rimz::harness::run::claim_rung(
        store.paths(),
        &run_id,
        jiff::Timestamp::now(),
        |record, rung| {
            let session = payload
                .get("session_id")
                .or_else(|| payload.get("sessionId"))
                .or_else(|| payload.get("conversation_id"))
                .and_then(Value::as_str);
            if record
                .agent_id
                .as_deref()
                .zip(session)
                .is_some_and(|(owner, session)| owner != session)
            {
                return false;
            }
            agent.attach_hook_context(&mut decoded, &rung.text())
        },
    ) {
        warn!(%run_id, %error, "hooks feed: failed to claim deadline context");
    }
    decoded.reply().clone()
}

fn emit_reply(reply: &rimz::agents::HookReply) -> Result<()> {
    if let rimz::agents::HookReply::Json(payload) = reply {
        let rendered = serde_json::to_string(payload)?;
        #[expect(clippy::print_stdout, reason = "hook stdout is the decision channel")]
        {
            println!("{rendered}");
        }
    }
    Ok(())
}
