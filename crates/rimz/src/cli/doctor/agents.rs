use std::fs;

use jiff::Timestamp;
use rimz::RuntimePaths;
use rimz::feed::AgentState;
use rimz::ids::ResolverId;
use rimz::resolver::Allowlist;
use rimz::trust::{self, TrustState};

use super::super::open_ledger;
use super::age_short;

/// Walk the snapshot's agent rollup and print one row per `(kind, agent_id)`
/// observed by `agent.lifecycle` events.
#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
pub(super) fn report_agent_rollup(ws: &rimz::ResolvedWorkspace, audit: bool) {
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

/// Report which agents have their Rimz hooks wired. A run in a Rimz room
/// registers nothing until the agent's real hook system invokes
/// `rimz hooks feed`, so this section distinguishes installed, installable,
/// and known-but-not-yet-installable adapters.
#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
pub(super) fn report_agent_hooks() {
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
                let untrusted = agent.untrusted_installed_hooks();
                if untrusted.is_empty() {
                    AgentHookDoctorStatus::Installed
                } else {
                    AgentHookDoctorStatus::InstalledUntrusted(untrusted)
                }
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
    for agent in rimz::agents::ADAPTERS {
        println!("  agent cover   : {}", coverage_summary(agent.descriptor()));
    }

    for (name, status) in &statuses {
        match status {
            AgentHookDoctorStatus::NotInstalled => {
                println!("  hooks install : run `rimz hooks install {name}` to wire {name} agents");
            }
            AgentHookDoctorStatus::InstalledUntrusted(events) => {
                println!(
                    "  hooks trust   : {name} silently skips untrusted hooks ({}) — {}",
                    events.join(", "),
                    rimz::agents::hook_trust_fix(name),
                );
            }
            AgentHookDoctorStatus::Unsupported(reason) => {
                println!("  hooks install : {name} unsupported ({reason})");
            }
            AgentHookDoctorStatus::Installed => {}
        };
    }
}

fn coverage_summary(descriptor: &rimz::agents::AgentDescriptor) -> String {
    let parts = rimz::agents::IntegrationConcern::ALL
        .iter()
        .filter_map(|concern| {
            descriptor.coverage.iter().find_map(|(declared, coverage)| {
                (declared == concern).then_some((*concern, *coverage))
            })
        })
        .map(|(concern, coverage)| match coverage {
            rimz::agents::ConcernCoverage::Wired { .. } => {
                format!("+{}", concern.short_label())
            }
            rimz::agents::ConcernCoverage::Unsupported { reason } => {
                format!(
                    "-{}({})",
                    concern.short_label(),
                    coverage_reason_text(reason)
                )
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("{} {parts}", descriptor.kind)
}

fn coverage_reason_text(reason: &str) -> String {
    const MAX_CHARS: usize = 32;

    let reason = reason.trim();
    let mut chars = reason.chars();
    let summary: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{summary}...")
    } else {
        summary
    }
}

enum AgentHookDoctorStatus {
    Installed,
    /// Installed, but the agent's own trust gate still skips these events.
    InstalledUntrusted(Vec<String>),
    NotInstalled,
    Unsupported(String),
}

impl AgentHookDoctorStatus {
    fn label(&self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::InstalledUntrusted(_) => "installed, untrusted",
            Self::NotInstalled => "not installed",
            Self::Unsupported(_) => "unsupported",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(kind: &str) -> &'static rimz::agents::AgentDescriptor {
        rimz::agents::descriptor_by_kind(kind).expect("registered descriptor")
    }

    #[test]
    fn coverage_summary_pins_agent_matrix() {
        assert_eq!(
            coverage_summary(descriptor("claude")),
            "claude +turn +perm +plan +ask +compact +sub +bg +end +idle +usage +rich +install +spend +remote"
        );
        assert_eq!(
            coverage_summary(descriptor("codex")),
            "codex +turn +perm -plan(no plan-approval gate; update_pl...) +ask +compact +sub -bg(no background-task parking) -end(no SessionEnd hook; liveness rea...) -idle(no idle Notification hook) +usage +rich +install +spend +remote"
        );
        assert_eq!(
            coverage_summary(descriptor("pi")),
            "pi +turn +perm -plan(no plan-approval gate) -ask(no native question tool) +compact -sub(no subagent hook surface) -bg(no background-task parking) +end -idle(no idle notification event) +usage -rich(no rich-context transport) +install +spend -remote(no remote-control surface)"
        );
    }

    #[test]
    fn coverage_reason_text_truncates_without_inspecting_words() {
        assert_eq!(coverage_reason_text("short reason"), "short reason");
        assert_eq!(
            coverage_reason_text("abcdefghijklmnopqrstuvwxyz0123456789"),
            "abcdefghijklmnopqrstuvwxyz012345..."
        );
    }
}

/// Surface the project-trust state. Stale is the case worth seeing in
/// `doctor`: the executable surface drifted since the last grant and
/// command-running fields are inert until `rimz trust grant` runs again.
#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
pub(super) fn report_trust(ws: &rimz::ResolvedWorkspace) {
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
/// dropped by the bridge per `docs/internals/agents/resolvers.md:35` but kept for
/// diagnostics so a user installing a resolver wrong sees why it's not
/// engaging.
#[expect(
    clippy::print_stdout,
    reason = "doctor is the user-facing report; called from a print_stdout-allowed parent"
)]
pub(super) fn report_unauthorized_resolver_heartbeats(ws: &rimz::ResolvedWorkspace) {
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
