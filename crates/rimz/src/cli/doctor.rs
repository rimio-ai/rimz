use std::fs;

use anyhow::Result;
use jiff::Timestamp;

use super::{GlobalFlags, open_ledger};
use rimz::feed::AgentState;
use rimz::ids::{MuxName, ResolverId};
use rimz::mux::{
    tmux::{self as tmux_mod, MIN_TMUX_VERSION},
    zellij::{self as zellij_mod, MIN_ZELLIJ_VERSION},
};
use rimz::resolver::Allowlist;
use rimz::workspace::WorkspaceResolver;
use rimz::{RuntimePaths, StatePaths};

/// AF_UNIX paths are 108 bytes including the terminator. The per-request
/// socket layout is `<sock_dir>/feed.<12-hex>.sock` (18 chars after
/// `sock_dir`), so the dir itself can be at most 89 bytes. We round down
/// to 88 to leave a byte of headroom.
const AF_UNIX_PATH_LIMIT: usize = 108;
const FEED_SOCKET_TAIL_LEN: usize = "/feed.123456789012.sock".len();

pub fn run(globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve(".", globals.root.clone());

    #[expect(clippy::print_stdout, reason = "doctor is the user-facing report")]
    {
        println!("Rimz doctor");
        match &workspace {
            Ok(ws) => {
                println!("  workspace id  : {}", ws.workspace_id);
                println!("  project root  : {}", ws.project_root.display());
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
            }
            Err(err) => println!("  multiplexer   : unavailable ({err})"),
        }

        if let Ok(ws) = &workspace {
            report_unauthorized_resolver_heartbeats(ws);
            report_agent_rollup(ws);
        }
    }
    Ok(())
}

/// Walk the snapshot's agent rollup and print one row per `(kind, agent_id)`
/// observed by `agent.lifecycle` events. The mode pill reflects the agent's
/// own most recent observation — the per-agent unattended-runs audit story
/// from `docs/guide/product.md` is the user-facing context.
#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
fn report_agent_rollup(ws: &rimz::ResolvedWorkspace) {
    let ledger = match open_ledger(ws) {
        Ok(l) => l,
        Err(err) => {
            println!("  agents        : unavailable ({err})");
            return;
        }
    };
    let snapshot = match ledger.snapshot() {
        Ok(s) => s,
        Err(err) => {
            println!("  agents        : unavailable ({err})");
            return;
        }
    };
    if snapshot.agents.is_empty() {
        println!("  agents        : none observed");
        return;
    }
    let now = Timestamp::now();
    let mut by_kind: std::collections::BTreeMap<&str, Vec<&AgentState>> =
        std::collections::BTreeMap::new();
    for agent in &snapshot.agents {
        by_kind.entry(agent.kind.as_str()).or_default().push(agent);
    }
    for (kind, mut agents) in by_kind {
        agents.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
        println!("  agent ({kind})  : {} observed", agents.len());
        for agent in agents {
            let mode = format!("{:?}", agent.mode).to_lowercase();
            let status = format!("{:?}", agent.status).to_lowercase();
            let branch = agent.worktree_branch.as_deref().unwrap_or("-");
            let age = age_short(now, agent.last_seen);
            println!(
                "    {id:<24} {branch:<20} {status:<8} · {mode:<7} · {age}",
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
            let presence = if caps.plugin_present {
                "present"
            } else {
                "missing"
            };
            println!(
                "  sidebar plugin: {} {presence}",
                caps.plugin_path.display(),
            );
        }
        Err(err) => println!("  zellij floor  : unavailable ({err})"),
    }
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
    let Ok(state) = StatePaths::for_workspace(ws.workspace_id.clone()) else {
        return;
    };
    let _ = state;
    let runtime = match RuntimePaths::for_workspace(ws.workspace_id.clone()) {
        Ok(r) => r,
        Err(err) => {
            println!("  sock headroom : unavailable ({err})");
            return;
        }
    };
    let dir_len = runtime.sock_dir.as_os_str().len();
    let total = dir_len + FEED_SOCKET_TAIL_LEN;
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
