//! `rimz doctor` — workspace health report: trust state, protocol versions, resolver freshness, socket-path budget, and the agent rollup.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use clap::Args;
use jiff::Timestamp;

use super::{GlobalFlags, open_ledger};
use rimz::bridge::AF_UNIX_PATH_LIMIT;
use rimz::config::MachineConfig;
use rimz::feed::AgentState;
use rimz::ids::{MuxName, ResolverId};
use rimz::ledger::event_log;
use rimz::mux::{
    MuxBackend, SessionHealth,
    tmux::{self as tmux_mod, MIN_TMUX_VERSION},
    zellij::{self as zellij_mod, MIN_ZELLIJ_VERSION},
};
use rimz::resolver::Allowlist;
use rimz::schema::{EVENT_SCHEMA_VERSION, RESOLVER_PROTOCOL_VERSION, SIDEBAR_PROTOCOL_VERSION};
use rimz::trust::{self, TrustState};
use rimz::workspace::WorkspaceResolver;
use rimz::{RuntimePaths, StatePaths};

#[derive(Debug, Args)]
pub struct DoctorArgs {
    #[arg(long)]
    audit: bool,
}

/// The longest socket name under `sock_dir` is the sidebar wakeup socket
/// `<sock_dir>/sidebar.<12-hex>.sock`; the per-request `feed.<12-hex>.sock`
/// is shorter. The budget itself is [`AF_UNIX_PATH_LIMIT`] — one authority
/// shared with the bridge binder's fail-fast precondition.
const LONGEST_SOCKET_TAIL_LEN: usize = "/sidebar.123456789012.sock".len();

pub fn run(args: DoctorArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve(".", globals.root.clone());

    #[expect(clippy::print_stdout, reason = "doctor is the user-facing report")]
    {
        println!("Rimz doctor");
        match &workspace {
            Ok(ws) => {
                println!("  workspace id  : {}", ws.workspace_id);
                println!("  project root  : {}", ws.project_root.display());
                println!("  root class    : {}", ws.root_class.label());
                println!("  worktree root : {}", ws.worktree_root.display());
                println!(
                    "  worktree branch: {}",
                    ws.worktree_branch.as_deref().unwrap_or("<detached>")
                );
                println!("  session name  : {}", ws.session_name);
                report_socket_headroom(ws);
            }
            Err(err) => println!("  workspace     : could not resolve ({err})"),
        }

        match rimz::mux::auto_detect_backend(globals.mux) {
            Ok(mux) => {
                println!("  multiplexer   : {mux}");
                let backend = rimz::mux::backend_for(mux);
                match backend.version() {
                    Ok(v) if !v.is_empty() => println!("  version       : {v}"),
                    Ok(_) => println!("  version       : unknown"),
                    Err(err) => println!("  version       : unavailable ({err})"),
                }
                match mux {
                    MuxName::Zellij => report_zellij_capabilities(),
                    MuxName::Tmux => report_tmux_capabilities(),
                }
                if let Ok(ws) = &workspace {
                    report_session_health(backend.as_ref(), &ws.session_name);
                }
            }
            Err(err) => println!("  multiplexer   : unavailable ({err})"),
        }
        report_sidebar_renderer();
        report_agent_hooks();
        report_remote_control();
        report_room_tree(workspace.as_ref().ok());

        if let Ok(ws) = &workspace {
            report_protocol_versions(ws);
            report_trust(ws);
            report_unauthorized_resolver_heartbeats(ws);
            report_agent_rollup(ws, args.audit);
        }
    }
    Ok(())
}

#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
fn report_protocol_versions(ws: &rimz::ResolvedWorkspace) {
    println!(
        "  protocols     : event {EVENT_SCHEMA_VERSION}; sidebar {SIDEBAR_PROTOCOL_VERSION}; resolver {RESOLVER_PROTOCOL_VERSION}",
    );
    report_event_schema_versions(ws);
    report_heartbeat_protocol_versions(ws);
}

#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
fn report_event_schema_versions(ws: &rimz::ResolvedWorkspace) {
    let paths = match StatePaths::for_workspace(ws.workspace_id.clone()) {
        Ok(paths) => paths,
        Err(err) => {
            println!("  protocol warn : event log unavailable ({err})");
            return;
        }
    };
    let events = match event_log::read_all(&paths.events_log) {
        Ok(events) => events,
        // Mid-file corruption — the post-power-cut corpse. Doctor stays
        // read-only; the truncating repair is gc's job.
        Err(err) if err.is_corruption() => {
            println!("  protocol warn : event log needs repair ({err}); run `rimz gc`");
            return;
        }
        Err(err) => {
            println!("  protocol warn : event log unavailable ({err})");
            return;
        }
    };
    let mut mismatches: BTreeMap<String, usize> = BTreeMap::new();
    for event in events {
        if event.schema_version != EVENT_SCHEMA_VERSION {
            *mismatches.entry(event.schema_version).or_default() += 1;
        }
    }
    for (version, count) in mismatches {
        let noun = if count == 1 { "record" } else { "records" };
        println!(
            "  protocol warn : event log schema {version} seen {count} {noun} (expected {EVENT_SCHEMA_VERSION})",
        );
    }
}

#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
fn report_heartbeat_protocol_versions(ws: &rimz::ResolvedWorkspace) {
    let runtime = match RuntimePaths::for_workspace(ws.workspace_id.clone()) {
        Ok(runtime) => runtime,
        Err(err) => {
            println!("  protocol warn : heartbeat dir unavailable ({err})");
            return;
        }
    };
    let entries = match fs::read_dir(&runtime.heartbeat_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(err) => {
            println!("  protocol warn : heartbeat dir unavailable ({err})");
            return;
        }
    };
    let mut checks = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some((kind, expected)) = heartbeat_kind_and_protocol(name) else {
            continue;
        };
        checks.push((name.to_owned(), kind, expected, path));
    }
    checks.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, kind, expected, path) in checks {
        match heartbeat_protocol_version(&path) {
            Ok(found) if found == expected => {}
            Ok(found) => println!(
                "  protocol warn : {kind} heartbeat {name} uses {found} (expected {expected})",
            ),
            Err(err) => {
                println!("  protocol warn : {kind} heartbeat {name} unreadable ({err})");
            }
        }
    }
}

fn heartbeat_kind_and_protocol(name: &str) -> Option<(&'static str, &'static str)> {
    if name.starts_with("sidebar.") && name.ends_with(".json") {
        Some(("sidebar", SIDEBAR_PROTOCOL_VERSION))
    } else if name.starts_with("resolver.") && name.ends_with(".json") {
        Some(("resolver", RESOLVER_PROTOCOL_VERSION))
    } else {
        None
    }
}

fn heartbeat_protocol_version(path: &Path) -> std::result::Result<String, String> {
    let bytes = fs::read(path).map_err(|err| err.to_string())?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|err| err.to_string())?;
    Ok(value
        .get("protocol_version")
        .and_then(|value| value.as_str())
        .unwrap_or("<missing>")
        .to_owned())
}

/// Walk the snapshot's agent rollup and print one row per `(kind, agent_id)`
/// observed by `agent.lifecycle` events.
#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
fn report_agent_rollup(ws: &rimz::ResolvedWorkspace, audit: bool) {
    let ledger = match open_ledger(ws) {
        Ok(l) => l,
        Err(err) => {
            println!("  agents        : unavailable ({err})");
            return;
        }
    };
    let scope = if audit {
        rimz::RuntimeScope::Audit
    } else {
        rimz::RuntimeScope::Runtime
    };
    let projection = match ledger.runtime_projection(scope) {
        Ok(s) => s,
        Err(err) => {
            println!("  agents        : unavailable ({err})");
            return;
        }
    };
    if projection.agents.is_empty() {
        println!("  agents        : none observed");
        return;
    }
    let now = Timestamp::now();
    let mut by_kind: std::collections::BTreeMap<&str, Vec<&AgentState>> =
        std::collections::BTreeMap::new();
    for agent in &projection.agents {
        by_kind.entry(agent.kind.as_str()).or_default().push(agent);
    }
    for (kind, mut agents) in by_kind {
        agents.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
        println!("  agent ({kind})  : {} observed", agents.len());
        for agent in agents {
            let status = format!("{:?}", agent.status).to_lowercase();
            let branch = agent.worktree_branch.as_deref().unwrap_or("-");
            let age = age_short(now, agent.last_seen);
            println!(
                "    {id:<24} {branch:<20} {status:<8} · {age}",
                id = agent.agent_id,
            );
        }
    }
}

fn age_short(now: Timestamp, then: Timestamp) -> String {
    let span = now.duration_since(then);
    if span.is_negative() {
        return "now".to_owned();
    }
    let secs = span.as_secs().max(0) as u64;
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
fn report_session_health(backend: &dyn MuxBackend, session_name: &str) {
    match backend.probe_session_health(session_name) {
        // `probe_session_health` never returns `Reborn` (it does not mutate), so
        // the live verdict is just clean-or-stuck.
        Ok(SessionHealth::Healthy | SessionHealth::Reborn) => println!("  session health: ok"),
        Ok(SessionHealth::Stuck) => println!(
            "  session health: stuck (resurrected/suspended panes) — run `rimz reset` to rebuild",
        ),
        Err(err) => println!("  session health: unavailable ({err})"),
    }
}

#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
fn report_zellij_capabilities() {
    match zellij_mod::capabilities() {
        Ok(caps) => {
            let floor_status = if caps.meets_min_version {
                "OK"
            } else {
                "TOO OLD"
            };
            let (maj, min, patch) = MIN_ZELLIJ_VERSION;
            println!("  zellij floor  : {floor_status} (>= {maj}.{min}.{patch} required)");
        }
        Err(err) => println!("  zellij floor  : unavailable ({err})"),
    }
}

/// Report which agents have their Rimz hooks wired. A run in a Rimz room
/// registers nothing until the agent's real hook system invokes
/// `rimz hooks feed`, so this section distinguishes installed, installable,
/// and known-but-not-yet-installable adapters.
#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
fn report_agent_hooks() {
    let statuses: Vec<(&str, AgentHookDoctorStatus)> = rimz::agents::ADAPTERS
        .iter()
        .map(|agent| {
            let descriptor = agent.descriptor();
            let status = if !descriptor.capabilities.hook_install {
                AgentHookDoctorStatus::Unsupported(
                    descriptor
                        .hook_install_unavailable
                        .unwrap_or("hook install is not supported for this adapter")
                        .to_owned(),
                )
            } else if agent.hooks_installed() {
                AgentHookDoctorStatus::Installed
            } else {
                AgentHookDoctorStatus::NotInstalled
            };
            (descriptor.kind, status)
        })
        .collect();

    let summary = statuses
        .iter()
        .map(|(name, status)| format!("{name} {}", status.label()))
        .collect::<Vec<_>>()
        .join("; ");
    println!("  agent hooks   : {summary}");

    for (name, status) in &statuses {
        match status {
            AgentHookDoctorStatus::NotInstalled => {
                println!("  hooks install : run `rimz hooks install {name}` to wire {name} agents");
            }
            AgentHookDoctorStatus::Unsupported(reason) => {
                println!("  hooks install : {name} unsupported ({reason})");
            }
            AgentHookDoctorStatus::Installed => {}
        };
    }
}

enum AgentHookDoctorStatus {
    Installed,
    NotInstalled,
    Unsupported(String),
}

impl AgentHookDoctorStatus {
    fn label(&self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::NotInstalled => "not installed",
            Self::Unsupported(_) => "unsupported",
        }
    }
}

/// Report the per-machine remote-control auto-launch posture. Codex's host has a
/// hard precondition — the managed standalone install — that `rimz start`
/// enforces fail-fast, so doctor surfaces the same gap and the same fix ahead of
/// time. Claude's host is best-effort (gated on PATH), so it only warns.
#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
fn report_remote_control() {
    let config = match MachineConfig::load() {
        Ok(config) => config.remote_control,
        Err(err) => {
            println!("  remote control: config unavailable ({err})");
            return;
        }
    };
    if !config.claude && !config.codex {
        println!("  remote control: off");
        return;
    }

    let codex_standalone_missing =
        config.codex && rimz::remote_control::codex_standalone_bin().is_none();
    let mut parts = Vec::new();
    if config.claude {
        parts.push(if which::which("claude").is_ok() {
            "claude ready".to_owned()
        } else {
            "claude enabled, not on PATH".to_owned()
        });
    }
    if config.codex {
        parts.push(if codex_standalone_missing {
            "codex enabled, standalone install missing".to_owned()
        } else {
            "codex ready".to_owned()
        });
    }
    println!("  remote control: {}", parts.join("; "));

    if codex_standalone_missing {
        println!(
            "  remote control: `rimz start` refuses until the managed standalone Codex install exists — {}",
            rimz::remote_control::CODEX_INSTALL_COMMAND,
        );
    }
}

#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
fn report_sidebar_renderer() {
    let status = if super::sidebar::sidebar_renderer_present() {
        "found"
    } else {
        "missing"
    };
    println!("  sidebar renderer: rimz-sidebar {status}");
}

#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
fn report_tmux_capabilities() {
    match tmux_mod::capabilities() {
        Ok(caps) => {
            let floor_status = if caps.meets_min_version {
                "OK"
            } else {
                "TOO OLD"
            };
            let (maj, min, patch) = MIN_TMUX_VERSION;
            println!("  tmux floor    : {floor_status} (>= {maj}.{min}.{patch} required)");
            let popup_status = if caps.popup_supported {
                "supported"
            } else {
                "unavailable (requires tmux >= 3.2)"
            };
            println!("  tmux popup    : {popup_status}");
        }
        Err(err) => println!("  tmux floor    : unavailable ({err})"),
    }
}

#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
fn report_socket_headroom(ws: &rimz::ResolvedWorkspace) {
    if StatePaths::for_workspace(ws.workspace_id.clone()).is_err() {
        return;
    }
    let runtime = match RuntimePaths::for_workspace(ws.workspace_id.clone()) {
        Ok(r) => r,
        Err(err) => {
            println!("  sock headroom : unavailable ({err})");
            return;
        }
    };
    let dir_len = runtime.sock_dir.as_os_str().len();
    let total = dir_len + LONGEST_SOCKET_TAIL_LEN;
    let status = if total < AF_UNIX_PATH_LIMIT {
        "OK"
    } else {
        "TIGHT"
    };
    println!(
        "  sock headroom : {status} ({total}/{AF_UNIX_PATH_LIMIT} bytes for {})",
        runtime.sock_dir.display(),
    );
}

/// The machine's room tree: every recorded workspace with its root, root
/// class, and liveness, the current directory's room starred. Live rooms
/// whose roots nest earn an overlap line — legal by design (an agent belongs
/// to the room its pane lives in), surfaced so the human always sees it.
#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
fn report_room_tree(current: Option<&rimz::ResolvedWorkspace>) {
    let known = match rimz::workspace::known_workspaces() {
        Ok(known) => known,
        Err(err) => {
            println!("  rooms         : unavailable ({err})");
            return;
        }
    };
    if known.is_empty() {
        println!("  rooms         : none recorded");
        return;
    }
    let live = super::live_session_names();
    let live_count = known
        .iter()
        .filter(|ws| live.contains(&ws.session_name))
        .count();
    println!(
        "  rooms         : {} recorded, {live_count} live",
        known.len()
    );
    let mut rooms: Vec<_> = known.iter().collect();
    rooms.sort_by(|a, b| a.project_root.cmp(&b.project_root));
    for ws in &rooms {
        let liveness = if live.contains(&ws.session_name) {
            "live"
        } else {
            "idle"
        };
        let here = if current.is_some_and(|cur| cur.workspace_id == ws.workspace_id) {
            "* "
        } else {
            "  "
        };
        println!(
            "    {here}{session}  {root} ({class}) · {liveness}",
            session = ws.session_name,
            root = ws.project_root.display(),
            class = ws.root_class.label(),
        );
    }
    for (i, a) in rooms.iter().enumerate() {
        for b in rooms.iter().skip(i + 1) {
            if !(live.contains(&a.session_name) && live.contains(&b.session_name)) {
                continue;
            }
            if rimz::workspace::root_contains(&a.project_root, &b.project_root)
                || rimz::workspace::root_contains(&b.project_root, &a.project_root)
            {
                println!(
                    "  rooms overlap : `{}` and `{}` nest; an agent belongs to the room its pane lives in",
                    a.session_name, b.session_name,
                );
            }
        }
    }
}

/// Surface the project-trust state. Stale is the case worth seeing in
/// `doctor`: the executable surface drifted since the last grant and
/// command-running fields are inert until `rimz trust grant` runs again.
#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
fn report_trust(ws: &rimz::ResolvedWorkspace) {
    let report = match trust::status(&ws.project_root) {
        Ok(report) => report,
        Err(err) => {
            println!("  trust         : unavailable ({err})");
            return;
        }
    };
    match report.state {
        TrustState::NoConfig => println!("  trust         : no project config"),
        TrustState::Untrusted => {
            println!(
                "  trust         : untrusted (run `rimz trust grant` to enable command paths)"
            );
        }
        TrustState::Trusted => {
            let at = report
                .granted_at
                .map(|t| t.to_string())
                .unwrap_or_else(|| "<unknown>".to_owned());
            println!("  trust         : trusted (granted {at})");
        }
        TrustState::Stale => {
            println!(
                "  trust         : stale (executable surface drifted; run `rimz trust grant` to refresh)",
            );
        }
    }
}

/// Walk the workspace's heartbeat dir and warn for any resolver-shaped
/// heartbeat whose id is not on the per-machine allowlist. These are
/// dropped by the bridge per `docs/internals/resolvers.md:35` but kept for
/// diagnostics so a user installing a resolver wrong sees why it's not
/// engaging.
#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
fn report_unauthorized_resolver_heartbeats(ws: &rimz::ResolvedWorkspace) {
    let runtime = match RuntimePaths::for_workspace(ws.workspace_id.clone()) {
        Ok(r) => r,
        Err(err) => {
            println!("  resolver hb   : unavailable ({err})");
            return;
        }
    };
    let allowlist = match Allowlist::load() {
        Ok(a) => a,
        Err(err) => {
            println!("  resolver hb   : allowlist unavailable ({err})");
            return;
        }
    };
    let entries = match fs::read_dir(&runtime.heartbeat_dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut unauthorized: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(stem) = name
            .strip_prefix("resolver.")
            .and_then(|s| s.strip_suffix(".json"))
        else {
            continue;
        };
        let Ok(id) = stem.parse::<ResolverId>() else {
            continue;
        };
        if !allowlist.contains(&id) {
            unauthorized.push(id.as_str().to_owned());
        }
    }
    if unauthorized.is_empty() {
        return;
    }
    unauthorized.sort();
    for id in unauthorized {
        println!("  resolver hb   : unauthorized resolver heartbeat seen ({id})");
    }
}

pub fn ping() -> Result<()> {
    #[expect(clippy::print_stdout, reason = "liveness check output")]
    {
        println!("ok");
    }
    Ok(())
}
