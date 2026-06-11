//! `rimz doctor` — workspace health report: trust state, protocol versions, resolver freshness, socket-path budget, and the agent rollup.

use anyhow::Result;
use clap::Args;
use jiff::Timestamp;

use super::GlobalFlags;
use rimz::ids::MuxName;
use rimz::workspace::WorkspaceResolver;

mod agents;
mod protocol;
mod runtime;

use agents::{
    report_agent_hooks, report_agent_rollup, report_trust, report_unauthorized_resolver_heartbeats,
};
use protocol::report_protocol_versions;
use runtime::{
    report_presence_channel, report_recent_diagnostics, report_remote_control, report_room_tree,
    report_session_health, report_sidebar_pane, report_socket_headroom, report_tmux_capabilities,
    report_zellij_capabilities, report_zellij_socket_headroom,
};
#[derive(Debug, Args)]
pub struct DoctorArgs {
    #[arg(long)]
    audit: bool,
}

/// The longest socket name under `sock_dir` is the sidebar wakeup socket
/// `<sock_dir>/sidebar.<12-hex>.sock`; the per-request `feed.<12-hex>.sock`
/// is shorter. The budget itself is [`AF_UNIX_PATH_LIMIT`] — one authority
/// shared with the bridge binder's fail-fast precondition.
pub(super) const LONGEST_SOCKET_TAIL_LEN: usize = "/sidebar.123456789012.sock".len();

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
                    if mux == MuxName::Zellij {
                        report_zellij_socket_headroom(ws);
                    }
                    report_session_health(backend.as_ref(), &ws.session_name);
                    if mux == MuxName::Zellij {
                        report_presence_channel(ws);
                    }
                }
            }
            Err(err) => println!("  multiplexer   : unavailable ({err})"),
        }
        report_sidebar_pane();
        report_agent_hooks();
        report_remote_control();
        report_room_tree(workspace.as_ref().ok());

        if let Ok(ws) = &workspace {
            report_protocol_versions(ws);
            report_trust(ws);
            report_unauthorized_resolver_heartbeats(ws);
            report_agent_rollup(ws, args.audit);
            report_recent_diagnostics(ws);
        }
    }
    Ok(())
}

pub(super) fn age_short(now: Timestamp, then: Timestamp) -> String {
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

pub fn ping() -> Result<()> {
    #[expect(clippy::print_stdout, reason = "liveness check output")]
    {
        println!("ok");
    }
    Ok(())
}
