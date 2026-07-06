use rimz::agents::AgentStatus;
use rimz::trust::{self};

use super::super::open_ledger;
use super::model::{AgentCounts, AgentRollup, AgentRow, HookRow, HookStatus, Probe, Trust};

/// Walk the snapshot's agent rollup into health counts and problem rows. The
/// default scope is live runtime state; audit widens to durable history and
/// emits every observed row.
pub(super) fn collect_agent_rollup(ws: &rimz::ResolvedWorkspace, audit: bool) -> AgentRollup {
    let ledger = match open_ledger(ws) {
        Ok(ledger) => ledger,
        Err(err) => {
            return AgentRollup::Unavailable {
                error: err.to_string(),
            };
        }
    };
    let scope = if audit {
        rimz::RuntimeScope::Audit
    } else {
        rimz::RuntimeScope::Runtime
    };
    let projection = match ledger.runtime_projection(scope) {
        Ok(projection) => projection,
        Err(err) => {
            return AgentRollup::Unavailable {
                error: err.to_string(),
            };
        }
    };
    if projection.agents.is_empty() {
        return AgentRollup::None;
    }
    let mut counts = AgentCounts::default();
    for agent in &projection.agents {
        counts.add(agent.status);
    }
    let mut agents: Vec<_> = projection.agents.iter().collect();
    agents.sort_by(|left, right| {
        left.kind
            .as_str()
            .cmp(right.kind.as_str())
            .then_with(|| left.agent_id.as_str().cmp(right.agent_id.as_str()))
    });
    let rows = agents
        .into_iter()
        .filter(|agent| audit || matches!(agent.status, AgentStatus::Failed | AgentStatus::Paused))
        .map(|agent| AgentRow {
            kind: agent.kind.as_str().to_owned(),
            agent_id: agent.agent_id.as_str().to_owned(),
            branch: agent.worktree_branch.clone(),
            status: agent.status,
            phase: agent.phase,
            last_seen: agent.last_seen,
        })
        .collect();
    AgentRollup::Observed { counts, rows }
}

/// Each adapter's Rimz-hook wiring state. A run in a Rimz room registers nothing
/// until the agent's own hook system invokes `rimz hooks feed`, so this
/// distinguishes installed, installable, and known-but-not-installable adapters.
pub(super) fn collect_hooks() -> Vec<HookRow> {
    rimz::agents::ADAPTERS
        .iter()
        .map(|agent| {
            let descriptor = agent.descriptor();
            let name = descriptor.kind;
            let status = if !descriptor.capabilities.hook_install {
                HookStatus::Unsupported {
                    reason: descriptor
                        .hook_install_unavailable
                        .unwrap_or("hook install is not supported for this adapter")
                        .to_owned(),
                }
            } else if agent.hooks_installed() {
                let untrusted = agent.untrusted_installed_hooks();
                if untrusted.is_empty() {
                    HookStatus::Installed
                } else {
                    HookStatus::InstalledUntrusted {
                        events: untrusted,
                        fix: rimz::agents::hook_trust_fix(name),
                    }
                }
            } else {
                HookStatus::NotInstalled {
                    fix: format!("run `rimz hooks install {name}` to wire {name} agents"),
                }
            };
            HookRow {
                kind: name.to_owned(),
                status,
            }
        })
        .collect()
}

/// Project-trust state. `Stale` is the case worth seeing: the executable surface
/// drifted since the last grant, so command-running fields are inert until
/// `rimz trust grant` runs again.
pub(super) fn collect_trust(ws: &rimz::ResolvedWorkspace) -> Probe<Trust> {
    match trust::status(&ws.project_root) {
        Ok(report) => Probe::Ready(Trust {
            state: report.state,
            granted_at: report.granted_at.map(|at| at.to_string()),
        }),
        Err(err) => Probe::Unavailable {
            error: err.to_string(),
        },
    }
}
