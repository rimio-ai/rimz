use std::fs;

use rimz::RuntimePaths;
use rimz::agents::{ConcernCoverage, HookCoverage, IntegrationConcern, LifecycleSignalKind};
use rimz::ids::ResolverId;
use rimz::resolver::Allowlist;
use rimz::trust::{self};

use super::super::open_ledger;
use super::model::{
    AgentKindGroup, AgentRollup, AgentRow, CoverageMatrix, HookRow, HookStatus, MatrixCell,
    MatrixRow, Probe, Trust,
};

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

/// Cross-adapter integration-concern coverage.
pub(super) fn collect_coverage() -> CoverageMatrix {
    let agents = matrix_agents();
    let mut rows = Vec::new();
    for concern in IntegrationConcern::ALL {
        let mut cells = Vec::new();
        for agent in rimz::agents::ADAPTERS {
            let descriptor = agent.descriptor();
            let coverage = concern_coverage(descriptor, concern);
            match coverage {
                ConcernCoverage::Wired { via } => cells.push(MatrixCell::ok(via)),
                ConcernCoverage::Partial { via, gap } => {
                    cells.push(MatrixCell::partial(format!("{via} — {gap}")));
                }
                ConcernCoverage::Unsupported { reason } => cells.push(MatrixCell::absent(reason)),
            }
        }
        rows.push(MatrixRow {
            label: concern.short_label().to_owned(),
            cells,
        });
    }
    CoverageMatrix { agents, rows }
}

/// Cross-adapter lifecycle-hook coverage.
pub(super) fn collect_hook_matrix() -> CoverageMatrix {
    let agents = matrix_agents();
    let mut rows = Vec::new();
    for signal_kind in LifecycleSignalKind::ALL {
        let mut cells = Vec::new();
        for agent in rimz::agents::ADAPTERS {
            let descriptor = agent.descriptor();
            let coverage = hook_coverage(descriptor, signal_kind);
            match coverage {
                HookCoverage::Native { event } => cells.push(MatrixCell::ok(event)),
                HookCoverage::Derived { via, gap } => {
                    cells.push(MatrixCell::partial(format!("{via} — {gap}")));
                }
                HookCoverage::Absent { reason } => cells.push(MatrixCell::absent(reason)),
            }
        }
        rows.push(MatrixRow {
            label: signal_kind.short_label().to_owned(),
            cells,
        });
    }
    CoverageMatrix { agents, rows }
}

fn matrix_agents() -> Vec<String> {
    rimz::agents::ADAPTERS
        .iter()
        .map(|agent| agent.descriptor().kind.to_owned())
        .collect()
}

fn concern_coverage(
    descriptor: &rimz::agents::AgentDescriptor,
    concern: IntegrationConcern,
) -> ConcernCoverage {
    descriptor
        .coverage
        .iter()
        .find(|(declared, _)| *declared == concern)
        .map(|(_, coverage)| *coverage)
        .unwrap_or(ConcernCoverage::Unsupported {
            reason: "coverage row missing",
        })
}

fn hook_coverage(
    descriptor: &rimz::agents::AgentDescriptor,
    signal_kind: LifecycleSignalKind,
) -> HookCoverage {
    descriptor
        .lifecycle_hooks
        .iter()
        .find(|(declared, _)| *declared == signal_kind)
        .map(|(_, coverage)| *coverage)
        .unwrap_or(HookCoverage::Absent {
            reason: "lifecycle hook row missing",
        })
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

#[cfg(test)]
mod tests {
    use super::super::model::MatrixCellState;
    use super::*;

    fn agent_cells(matrix: &CoverageMatrix, agent: &str) -> Vec<MatrixCellState> {
        let idx = matrix
            .agents
            .iter()
            .position(|kind| kind == agent)
            .expect("agent column");
        matrix.rows.iter().map(|row| row.cells[idx].state).collect()
    }

    fn agent_labels<'a>(
        matrix: &'a CoverageMatrix,
        agent: &str,
        state: MatrixCellState,
    ) -> Vec<&'a str> {
        let idx = matrix
            .agents
            .iter()
            .position(|kind| kind == agent)
            .expect("agent column");
        matrix
            .rows
            .iter()
            .filter(|row| row.cells[idx].state == state)
            .map(|row| row.label.as_str())
            .collect()
    }

    fn row<'a>(matrix: &'a CoverageMatrix, label: &str) -> &'a MatrixRow {
        matrix
            .rows
            .iter()
            .find(|row| row.label == label)
            .expect("matrix row")
    }

    fn cell_detail<'a>(matrix: &CoverageMatrix, row: &'a MatrixRow, agent: &str) -> &'a str {
        let idx = matrix
            .agents
            .iter()
            .position(|kind| kind == agent)
            .expect("agent column");
        row.cells[idx].detail.as_str()
    }

    fn states(row: &MatrixRow) -> Vec<MatrixCellState> {
        row.cells.iter().map(|cell| cell.state).collect()
    }

    fn count(cells: &[MatrixCellState], needle: MatrixCellState) -> usize {
        cells.iter().filter(|cell| **cell == needle).count()
    }

    #[test]
    fn coverage_pins_agent_matrix() {
        let matrix = collect_coverage();
        assert_eq!(matrix.agents, ["claude", "codex", "pi", "opencode"]);
        assert_eq!(matrix.rows.len(), IntegrationConcern::ALL.len());

        let claude = agent_cells(&matrix, "claude");
        assert_eq!(
            count(&claude, MatrixCellState::Ok),
            IntegrationConcern::ALL.len()
        );
        assert_eq!(count(&claude, MatrixCellState::Partial), 0);
        assert_eq!(count(&claude, MatrixCellState::Absent), 0);

        let codex = agent_cells(&matrix, "codex");
        assert_eq!(count(&codex, MatrixCellState::Ok), 11);
        assert_eq!(count(&codex, MatrixCellState::Partial), 2);
        assert_eq!(count(&codex, MatrixCellState::Absent), 2);
        // `end` and `idle` have no native hook, but pane liveness/the reaper and
        // the turn-boundary/stall path reconstruct them — partial, not absent.
        assert_eq!(
            agent_labels(&matrix, "codex", MatrixCellState::Partial),
            ["end", "idle"]
        );
        assert_eq!(
            agent_labels(&matrix, "codex", MatrixCellState::Absent),
            ["plan", "bg"]
        );
        assert!(cell_detail(&matrix, row(&matrix, "end"), "codex").contains("SessionEnd"));

        let pi = agent_cells(&matrix, "pi");
        assert_eq!(count(&pi, MatrixCellState::Ok), 7);
        assert_eq!(count(&pi, MatrixCellState::Partial), 2);
        assert_eq!(count(&pi, MatrixCellState::Absent), 6);
        // Pi has no idle Notification hook, but `agent_end` plus the stall
        // window reconstruct the attention slice — partial, like Codex, not
        // absent.
        assert_eq!(
            agent_labels(&matrix, "pi", MatrixCellState::Partial),
            ["idle", "live$"]
        );
        assert_eq!(
            agent_labels(&matrix, "pi", MatrixCellState::Absent),
            ["plan", "ask", "sub", "bg", "rich", "remote"]
        );
    }

    #[test]
    fn full_coverage_reports_no_gaps() {
        let matrix = collect_coverage();
        let claude = agent_cells(&matrix, "claude");
        assert_eq!(count(&claude, MatrixCellState::Ok), 15);
        assert_eq!(count(&claude, MatrixCellState::Partial), 0);
        assert_eq!(count(&claude, MatrixCellState::Absent), 0);
    }

    #[test]
    fn hook_matrix_pins_lifecycle_signals() {
        let matrix = collect_hook_matrix();
        assert_eq!(matrix.agents, ["claude", "codex", "pi", "opencode"]);
        assert_eq!(matrix.rows.len(), LifecycleSignalKind::ALL.len());

        let ended = row(&matrix, "ended");
        assert_eq!(
            states(ended),
            [
                MatrixCellState::Ok,
                MatrixCellState::Partial,
                MatrixCellState::Ok,
                MatrixCellState::Partial
            ]
        );
        assert!(cell_detail(&matrix, ended, "codex").contains("SessionEnd hook"));

        let subagent_started = row(&matrix, "subagent_started");
        assert_eq!(
            states(subagent_started),
            [
                MatrixCellState::Ok,
                MatrixCellState::Ok,
                MatrixCellState::Absent,
                MatrixCellState::Ok
            ]
        );
    }
}
