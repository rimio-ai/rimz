use std::fs;

use rimz::RuntimePaths;
use rimz::ids::ResolverId;
use rimz::resolver::Allowlist;
use rimz::trust::{self};

use super::super::open_ledger;
use super::model::{AgentKindGroup, AgentRollup, AgentRow, HookRow, HookStatus, Probe, Trust};

/// Walk the snapshot's agent rollup into one row per `(kind, agent_id)` observed
/// by `agent.lifecycle` events, grouped by kind.
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
    let mut by_kind: std::collections::BTreeMap<&str, Vec<&rimz::feed::AgentState>> =
        std::collections::BTreeMap::new();
    for agent in &projection.agents {
        by_kind.entry(agent.kind.as_str()).or_default().push(agent);
    }
    let groups = by_kind
        .into_iter()
        .map(|(kind, mut agents)| {
            agents.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
            AgentKindGroup {
                kind: kind.to_owned(),
                agents: agents
                    .into_iter()
                    .map(|agent| AgentRow {
                        agent_id: agent.agent_id.as_str().to_owned(),
                        branch: agent.worktree_branch.clone(),
                        status: agent.status,
                        phase: agent.phase,
                        last_seen: agent.last_seen,
                    })
                    .collect(),
            }
        })
        .collect();
    AgentRollup::Observed { groups }
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

/// Resolver-shaped heartbeats whose id is not on the per-machine allowlist. The
/// bridge drops these, so a user installing a resolver wrong sees why it never
/// engages.
pub(super) fn collect_unauthorized_resolvers(ws: &rimz::ResolvedWorkspace) -> Probe<Vec<String>> {
    let runtime = match RuntimePaths::for_workspace(ws.workspace_id.clone()) {
        Ok(runtime) => runtime,
        Err(err) => {
            return Probe::Unavailable {
                error: err.to_string(),
            };
        }
    };
    let allowlist = match Allowlist::load() {
        Ok(allowlist) => allowlist,
        Err(err) => {
            return Probe::Unavailable {
                error: format!("allowlist unavailable ({err})"),
            };
        }
    };
    let Ok(entries) = fs::read_dir(&runtime.heartbeat_dir) else {
        return Probe::Ready(Vec::new());
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
    unauthorized.sort();
    Probe::Ready(unauthorized)
}
