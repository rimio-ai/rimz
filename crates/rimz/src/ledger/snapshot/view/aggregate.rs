use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use tracing::warn;

use crate::agents::lifecycle::TurnPhase;
use crate::agents::{AgentContext, AgentTurnError, RateLimitWindow, TurnErrorClass};
use crate::feed::{AgentState, AgentStatus};
use crate::ids::AgentKind;
use crate::ledger::snapshot::row::{SidebarRow, SidebarSubAgent};
use crate::workspace::RootClass;

use super::layout::{
    capped_rows, cmp_start_asc, compare_groups, compare_rows, group_branch_label, status_counts,
    worktree_group_key,
};
use super::reap::GHOST_SESSION_TTL_SECS;
use super::{SidebarWorktreeGroup, SidebarWorktreeKind};

pub(super) fn build_worktree_groups_from_rows(
    mut rows: Vec<SidebarRow>,
    agents: &[AgentState],
    project_root: Option<&Path>,
    worktree_roots: &[PathBuf],
    root_class: RootClass,
    now: Timestamp,
    stalled_after_secs: u32,
) -> Vec<SidebarWorktreeGroup> {
    // Nest each subagent under its parent root row before grouping. This is the
    // one chokepoint every live (`rows_from_panes`) card flows through, so
    // nesting behaves identically for process, agent, and attention rows.
    attach_sub_agents(&mut rows, agents, now);
    // A delegating parent's work is its children's, so their activity advances
    // the parent row's displayed clock — before the projection below, so the
    // stall check reads the folded value too.
    fold_child_activity_onto_parents(&mut rows);
    // Project the displayed status now that each row knows its subagents (the
    // delegated-wait exemption) and the full agent set is in hand (the account
    // rate-limit verdict). The one place display state diverges from the rollup.
    project_display_status(&mut rows, agents, now, stalled_after_secs);
    // A worktree dir holds one branch at a time, so rows under one path
    // normally share a branch and group together — the agent and its shell
    // panes alike. Only when two live-admitted rows carry distinct branches
    // under one path do we split that path by branch, so a mislabeled
    // cross-branch section can't form while the common "agent + its shell" case
    // stays whole.
    let mut branches_per_path: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for row in &rows {
        if let (Some(path), Some(branch)) = (
            row.worktree_path.as_deref().filter(|path| !path.is_empty()),
            row.worktree_branch
                .as_deref()
                .filter(|branch| !branch.is_empty()),
        ) {
            branches_per_path.entry(path).or_default().insert(branch);
        }
    }
    let multi_branch_paths: BTreeSet<String> = branches_per_path
        .into_iter()
        .filter(|(_, branches)| branches.len() > 1)
        .map(|(path, _)| path.to_owned())
        .collect();

    let mut by_group: BTreeMap<String, (String, SidebarWorktreeKind, Vec<SidebarRow>)> =
        BTreeMap::new();
    for row in rows {
        let split_by_branch = row
            .worktree_path
            .as_deref()
            .is_some_and(|path| multi_branch_paths.contains(path));
        let (kind, key, label) = worktree_group_key(
            row.worktree_path.as_deref(),
            row.worktree_branch.as_deref(),
            split_by_branch,
            project_root,
            worktree_roots,
            root_class,
        );
        by_group
            .entry(key)
            .and_modify(|(_, _, rows)| rows.push(row.clone()))
            .or_insert_with(|| (label, kind, vec![row]));
    }

    let mut groups = by_group
        .into_iter()
        .map(|(key, (label, kind, mut rows))| {
            rows.sort_by(compare_rows);
            // Prefer a branch label over the path-basename seed: a group can mix
            // a branched agent row with a branchless process/attention row, and
            // every branched row in a group shares one branch (a path with two
            // is split above), so any branch is the right, order-independent
            // label. The root pod keeps its directory name — a non-repo root
            // has no branch, so a stale branched row must not rename the room.
            let label = if kind == SidebarWorktreeKind::Root {
                label
            } else {
                group_branch_label(&rows).unwrap_or(label)
            };
            let status_counts = status_counts(&rows);
            let total = rows.len();
            rows = capped_rows(rows);
            SidebarWorktreeGroup {
                key,
                label,
                kind,
                status_counts,
                hidden_count: total.saturating_sub(rows.len()),
                rows,
                diff_added: None,
                diff_removed: None,
                commits_ahead: None,
                commits_behind: None,
                trunk: None,
                clean: None,
            }
        })
        .collect::<Vec<_>>();
    groups.sort_by(compare_groups);
    groups
}

/// Nest each subagent under its parent root row. A subagent is a reduced
/// `AgentState` carrying `parent_agent_id`; it is paneless, so it built no row
/// of its own (`rows_from_panes` binds only stamped panes). This pass matches
/// each child to its parent row by
/// `(kind, parent_agent_id)` and pushes a compact summary onto it.
///
/// Retention is turn-scoped: a finished (success/failed) child stays listed
/// until its work predates the parent's *current* turn (`turn_started_at`,
/// advanced only by a turn start, the `TurnStarted` signal, never a turn end),
/// when it belongs to a past turn and is dropped. The generous
/// [`GHOST_SESSION_TTL_SECS`] backstop covers the no-turn-boundary case, so a
/// finished child cannot linger forever when the parent never recorded the next
/// turn start. A *running* child superseded by a newer parent turn, or silent
/// past that same backstop, is a ghost that never sent `Stop` — reaped so it
/// can't freeze the parent's delegated-wait head. A child whose parent row is
/// absent (parent ended, reaped, or has no live pane) is an orphan and never
/// renders. Survivors are deduped by child id so a child can never appear
/// twice, then ordered by creation time for a deterministic list.
pub(in crate::ledger::snapshot) fn attach_sub_agents(
    rows: &mut [SidebarRow],
    agents: &[AgentState],
    now: Timestamp,
) {
    let parent_turn_start = |kind: &str, id: &str| -> Option<Timestamp> {
        agents
            .iter()
            .find(|a| a.kind == kind && a.agent_id == id)
            .and_then(|a| a.turn_started_at)
    };
    let idle_secs = |child: &AgentState| now.duration_since(child.last_activity).as_secs();
    for child in agents.iter().filter(|a| a.parent_agent_id.is_some()) {
        let Some(parent_id) = child.parent_agent_id.as_deref() else {
            continue;
        };
        let parent_turn_started_at = parent_turn_start(&child.kind, parent_id);
        let parent_has_turn_boundary = parent_turn_started_at.is_some();
        let superseded =
            parent_turn_started_at.is_some_and(|started| started > child.last_activity);
        let keep = if child.status == AgentStatus::Running {
            if superseded {
                warn!(
                    target: "rimz::agent::lifecycle",
                    kind = %child.kind,
                    parent = parent_id,
                    child = %child.agent_id,
                    "running subagent superseded by a newer parent turn — reaped",
                );
                false
            } else if idle_secs(child) >= GHOST_SESSION_TTL_SECS {
                warn!(
                    target: "rimz::agent::lifecycle",
                    kind = %child.kind,
                    parent = parent_id,
                    child = %child.agent_id,
                    "subagent stuck running with no Stop past the ghost TTL — reaped",
                );
                false
            } else {
                true
            }
        } else {
            // Finished: turn-scoped — kept until the parent's next turn
            // supersedes it (its work predates `turn_started_at`). The
            // generous ghost TTL is the backstop for a parent that never
            // recorded a turn boundary, so a finished child can never linger
            // forever in the gap.
            !superseded && (parent_has_turn_boundary || idle_secs(child) < GHOST_SESSION_TTL_SECS)
        };
        if !keep {
            continue;
        }
        // Attach to the parent row when one is present; an orphan (no parent
        // row) never renders — but log it, since a child that names a parent
        // with no row is an anomaly worth tracing.
        let parent = rows
            .iter_mut()
            .filter(|row| row.name == child.kind && row.id == parent_id)
            .find_map(SidebarRow::as_agent_mut);
        if let Some(parent) = parent {
            parent.sub_agents.push(sub_agent_from_state(child, now));
        } else {
            warn!(
                target: "rimz::agent::lifecycle",
                kind = %child.kind,
                parent = parent_id,
                child = %child.agent_id,
                "subagent names a parent with no row — orphan, not rendered",
            );
        }
    }
    for agent in rows.iter_mut().filter_map(SidebarRow::as_agent_mut) {
        if agent.sub_agents.is_empty() {
            continue;
        }
        // Dedup by child id (freshest activity wins) so the same logical child
        // can never appear twice and the `subagents (N)` count stays honest.
        agent
            .sub_agents
            .sort_by(|a, b| a.id.cmp(&b.id).then(b.last_activity.cmp(&a.last_activity)));
        agent.sub_agents.dedup_by(|a, b| a.id == b.id);
        // Display order: creation time ascending — the spawn order the parent
        // launched them in, stable across refreshes (an activity-keyed sort
        // reshuffled the list on every tick). A child with no reported start
        // time sorts after the dated ones; the id tiebreak keeps the whole
        // order deterministic.
        agent.sub_agents.sort_by(|a, b| {
            cmp_start_asc(a.started_at, b.started_at).then_with(|| a.id.cmp(&b.id))
        });
    }
}

/// Advance each parent row's *displayed* `last_activity` to its freshest
/// child's: a delegating parent is quiet because the work is its children's,
/// so the age clock stays honest while they tick and a parent whose child just
/// finished never false-stalls (the stall check reads the folded clock).
/// Display-only — the rollup's own `last_activity` is untouched, so
/// `agent_moved_past_ask` keeps reading the agent's own clock and a blocked
/// parent stays waiting. Two guards keep the frozen clocks frozen: an
/// attention row (`waiting`/`failed`) measures how long it has needed a human,
/// and a turn that died on a provider error keeps its own clock so a
/// still-ticking child can never mask the death certificate.
fn fold_child_activity_onto_parents(rows: &mut [SidebarRow]) {
    for row in rows.iter_mut() {
        let Some(agent) = row.as_agent() else {
            continue;
        };
        if agent.sub_agents.is_empty() {
            continue;
        }
        let Some(status) = agent.status else {
            continue;
        };
        if matches!(status, AgentStatus::Waiting | AgentStatus::Failed) {
            continue;
        }
        if crate::feed::is_turn_dead(status, agent.context.as_ref(), row.last_activity) {
            continue;
        }
        if let Some(freshest) = agent
            .sub_agents
            .iter()
            .map(|child| child.last_activity)
            .max()
        {
            row.last_activity = row.last_activity.max(freshest);
        }
    }
}

/// Project each agent row's *displayed* status from its raw lifecycle status,
/// liveness, live subagents, turn-error marker, and provider budget windows.
/// This is the one place display state diverges from the rollup truth kept in
/// `snapshot.agents`; a pending ask already folded `waiting` onto the row
/// upstream and always wins.
///
/// Rows reaching this projection have already been admitted through a live mux
/// pane by `rows_from_panes`/`with_live_panes`, so no second liveness check is
/// needed here.
///
/// - A paused-class turn-error marker means the agent actually stopped
///   mid-turn on a provider limit. It projects to `paused`; a rate-limit marker
///   whose spent windows have provably reset escalates to `failed` so the row
///   asks for a resume nudge.
/// - A `running` agent with a live subagent is *waiting on its children*, not
///   wedged — unless a paused marker above says the provider stopped the turn.
/// - A failed-class turn-error marker projects to `failed` at once and carries
///   the upstream error text as `turn_error_label`.
/// - A stalled `running` agent whose kind still has a spent, unreset window
///   projects to `paused`; any other stall projects to `failed`.
fn project_display_status(
    rows: &mut [SidebarRow],
    agents: &[AgentState],
    now: Timestamp,
    stalled_after_secs: u32,
) {
    let rate_limit_kinds = rate_limit_window_kinds(agents, now);
    for row in rows.iter_mut() {
        let row_name = row.name.clone();
        let last_activity = row.last_activity;
        let Some(agent) = row.as_agent_mut() else {
            continue;
        };
        let Some(status) = agent.status else {
            continue;
        };
        // A human-blocked `waiting` ask outranks every derived state.
        if status == AgentStatus::Waiting {
            continue;
        }
        let has_live_child = agent
            .sub_agents
            .iter()
            .any(|child| child.status == AgentStatus::Running);
        let active_error = active_turn_error(status, agent.context.as_ref(), last_activity);
        let projected = if let Some(error) = active_error.filter(|error| {
            matches!(
                error.class,
                TurnErrorClass::PausedRateLimit | TurnErrorClass::PausedOverloaded
            )
        }) {
            if error.class == TurnErrorClass::PausedRateLimit
                && rate_limit_kinds.reset.contains(row_name.as_str())
                && !rate_limit_kinds.spent.contains(row_name.as_str())
            {
                agent.turn_error_label = error.label.clone();
                AgentStatus::Failed
            } else {
                AgentStatus::Paused
            }
        } else if status == AgentStatus::Running && has_live_child {
            AgentStatus::Running
        } else if let Some(error) =
            active_error.filter(|error| error.class == TurnErrorClass::Failed)
        {
            agent.turn_error_label = error.label.clone();
            AgentStatus::Failed
        } else {
            let stalled = crate::feed::is_stalled(status, last_activity, now, stalled_after_secs);
            if stalled && rate_limit_kinds.spent.contains(row_name.as_str()) {
                AgentStatus::Paused
            } else if stalled {
                AgentStatus::Failed
            } else {
                status
            }
        };
        agent.status = Some(projected);
        if projected != AgentStatus::Running {
            // Phase is a head on Running — the reduced state's invariant —
            // so a Failed/Paused override drops it rather than carrying
            // a stale Reasoning/Acting onto a resting row.
            agent.phase = TurnPhase::Idle;
        }
    }
}

fn active_turn_error(
    status: AgentStatus,
    context: Option<&AgentContext>,
    last_activity: Timestamp,
) -> Option<&AgentTurnError> {
    if status != AgentStatus::Running {
        return None;
    }
    context
        .and_then(|context| context.turn_error.as_ref())
        .filter(|error| error.at > last_activity)
}

#[derive(Default)]
struct RateLimitKindSummary {
    /// Provider kinds with a currently-spent budget window. This is not a
    /// parking verdict by itself; it only powers the stalled-running fallback.
    spent: BTreeSet<AgentKind>,
    /// Provider kinds whose known spent windows have passed their reset
    /// instant. A rate-limit pause marker uses this as proof that at least one
    /// wait ended; projection still requires no unreset spent window before
    /// lifting the pause.
    reset: BTreeSet<AgentKind>,
}

fn rate_limit_window_kinds(agents: &[AgentState], now: Timestamp) -> RateLimitKindSummary {
    let mut summary = RateLimitKindSummary::default();
    for agent in agents {
        if agent.parent_agent_id.is_some() {
            continue;
        }
        let Some(limits) = agent
            .context
            .as_ref()
            .and_then(|ctx| ctx.rate_limits.as_ref())
        else {
            continue;
        };
        let mut has_spent = false;
        let mut has_reset = false;
        for window in &limits.windows {
            if !window.is_spent() {
                continue;
            }
            if window_spent_unreset(window, now) {
                has_spent = true;
            } else {
                has_reset = true;
            }
        }
        if has_spent {
            summary.spent.insert(agent.kind.clone());
        }
        if has_reset {
            summary.reset.insert(agent.kind.clone());
        }
    }
    summary
}

/// Whether a window is spent and has not yet reset — the budget is gone *now*. A
/// spent window whose `resets_at` has already passed is stale, not limiting.
fn window_spent_unreset(window: &RateLimitWindow, now: Timestamp) -> bool {
    window.is_spent() && window.resets_at.is_none_or(|reset| reset > now)
}

/// A child `AgentState` projected to the compact summary the parent's expanded
/// card paints. The subagent's type rode in as its `task` on `SubagentStart`,
/// carried forward as identity by the reducer, so it stays labeled after it
/// finishes even when its `SubagentStop` omits the type. A child that somehow
/// reaches projection without a type is named by a short id placeholder, never
/// the provider `kind` (which would render as a phantom `claude`/`codex` row
/// indistinguishable from a real subagent), and the anomaly is logged. Elapsed
/// work is frozen at projection: a running child counts to `now`, a finished
/// one to its `last_activity` (which stops advancing), so the figure settles
/// when it ends.
pub(in crate::ledger::snapshot) fn sub_agent_from_state(
    child: &AgentState,
    now: Timestamp,
) -> SidebarSubAgent {
    let name = child
        .task
        .clone()
        .filter(|task| !task.is_empty())
        .unwrap_or_else(|| {
            warn!(
                target: "rimz::agent::lifecycle",
                kind = %child.kind,
                child = %child.agent_id,
                "subagent has no type label — rendering a degraded placeholder",
            );
            degraded_subagent_label(&child.agent_id)
        });
    let elapsed_secs = child.subagent_started_at.map(|started| {
        let until = if child.status == AgentStatus::Running {
            now
        } else {
            child.last_activity
        };
        until.duration_since(started).as_secs().max(0)
    });
    SidebarSubAgent {
        id: child.agent_id.to_string(),
        name,
        status: child.status,
        phase: child.phase,
        task: child.task.clone(),
        model: child.model.clone(),
        effort: child.effort.clone(),
        description: child.subagent_description.clone(),
        total_tokens: child.total_tokens,
        elapsed_secs,
        started_at: child.subagent_started_at,
        last_activity: child.last_activity,
    }
}

/// A placeholder label for a subagent that reported no type — a short id prefix
/// so it reads as a distinct, traceable child rather than the provider kind.
fn degraded_subagent_label(agent_id: &str) -> String {
    let short = agent_id.split('-').next().unwrap_or(agent_id);
    let short = short.get(..8).unwrap_or(short);
    if short.is_empty() {
        "subagent".to_owned()
    } else {
        format!("subagent {short}")
    }
}
