use std::collections::{BTreeMap, BTreeSet};

use crate::agent_activity::AgentActivity;
use crate::agents::{AccountBudget, AgentState};
use crate::diag::record::DiagEvent;
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::ledger::snapshot::panes::{
    LazyAgentPairingResult, pane_admits_card, row_from_frame_pane, stamped_agent_for_pane,
};
use crate::pane::PaneRef;

use super::SidebarSnapshot;
use super::aggregate::{AgentProjection, AttentionWindows, build_worktree_groups_from_rows};
use super::layout::{GroupRoots, refresh_overlay_group};
use projection::{LazyAgentPaneProjection, rows_from_panes};

mod projection;

#[cfg(test)]
pub(crate) use projection::{
    fold_ask_onto_row_for_test as fold_ask_onto_row, row_identity_violations,
};

impl SidebarSnapshot {
    /// Fold live multiplexer panes into the sidebar view-model. This reducer is
    /// pure: callers own pane discovery and pass the result in, so snapshot
    /// building stays independent of any backend command.
    pub fn with_live_panes(mut self, panes: Vec<PaneRef>, exclude: Option<&PaneId>) -> Self {
        let panes = Self::card_admitted_live_panes(panes, exclude);
        self.fold_admitted_live_panes(&panes, None, None, &BTreeMap::new(), &BTreeSet::new());
        self
    }

    #[cfg(test)]
    pub(crate) fn with_live_panes_and_account_budgets(
        mut self,
        panes: Vec<PaneRef>,
        exclude: Option<&PaneId>,
        account_budgets: &BTreeMap<AgentKind, AccountBudget>,
    ) -> Self {
        let panes = Self::card_admitted_live_panes(panes, exclude);
        self.fold_admitted_live_panes(&panes, None, None, account_budgets, &BTreeSet::new());
        self
    }

    pub fn card_admitted_live_panes(panes: Vec<PaneRef>, exclude: Option<&PaneId>) -> Vec<PaneRef> {
        panes
            .into_iter()
            .filter(|pane| pane_admits_card(pane, exclude).admits())
            .collect()
    }

    pub(crate) fn with_admitted_live_panes_and_diagnostics(
        mut self,
        panes: Vec<PaneRef>,
        lazy_pairings: &LazyAgentPairingResult,
        unread_row_ids: Option<&BTreeSet<String>>,
        account_budgets: &BTreeMap<AgentKind, AccountBudget>,
        exhausted_resumes: &BTreeSet<(AgentKind, AgentSessionId)>,
    ) -> (Self, Vec<DiagEvent>) {
        let diagnostics = self.fold_admitted_live_panes(
            &panes,
            Some(lazy_pairings),
            unread_row_ids,
            account_budgets,
            exhausted_resumes,
        );
        (self, diagnostics)
    }

    fn fold_admitted_live_panes(
        &mut self,
        panes: &[PaneRef],
        lazy_pairings: Option<&LazyAgentPairingResult>,
        unread_row_ids: Option<&BTreeSet<String>>,
        account_budgets: &BTreeMap<AgentKind, AccountBudget>,
        exhausted_resumes: &BTreeSet<(AgentKind, AgentSessionId)>,
    ) -> Vec<DiagEvent> {
        let mut projection = rows_from_panes(
            &self.agents,
            &self.needs_attention,
            &self.resolver_working,
            panes,
            LazyAgentPaneProjection {
                wired_kinds: &self.wired_kinds,
                default_models: &self.wired_default_models,
                pairings: lazy_pairings,
            },
            self.panes_observed_at_ms.or(self.panes_produced_at_ms),
            self.now,
        );
        if let Some(unread_row_ids) = unread_row_ids {
            stamp_unread_rows(&mut projection.rows, unread_row_ids);
        }
        self.agent_panes = projection.agent_panes;
        self.worktree_groups = build_worktree_groups_from_rows(
            projection.rows,
            AgentProjection {
                agents: &self.agents,
                account_budgets,
                exhausted_resumes,
            },
            GroupRoots {
                project_root: self.project_root.as_deref(),
                worktree_roots: &self.worktree_roots,
                worktree_home: self.worktree_home.as_deref(),
                root_class: self.root_class,
            },
            self.now,
            AttentionWindows::from_config(&self.attention),
        );
        projection.diagnostics
    }

    pub(crate) fn remove_pane_rows(&mut self, pane_id: &PaneId) -> bool {
        let mut changed = false;
        for group in &mut self.worktree_groups {
            let before = group.rows.len();
            group.rows.retain(|row| {
                !row.pane
                    .as_ref()
                    .is_some_and(|pane| pane.pane_id == *pane_id)
            });
            changed |= group.rows.len() != before;
            refresh_overlay_group(group);
        }
        self.worktree_groups.retain(|group| !group.rows.is_empty());
        if self
            .focused_pane
            .as_ref()
            .is_some_and(|focused| focused == pane_id)
        {
            self.focused_pane = None;
            changed = true;
        }
        changed
    }

    pub(crate) fn overlay_pane_command(&mut self, pane_id: &PaneId, command: &str) -> bool {
        let mut changed = false;
        for group in &mut self.worktree_groups {
            for row in &mut group.rows {
                let Some(pane) = row.pane.as_mut() else {
                    continue;
                };
                if pane.pane_id != *pane_id {
                    continue;
                }
                pane.command = Some(command.to_owned());
                pane.pane_process_start = None;
                if let Some(next) = row_from_frame_pane(
                    pane,
                    &self.wired_kinds,
                    &self.wired_default_models,
                    self.now,
                ) {
                    let worktree_path = row
                        .worktree_path
                        .clone()
                        .or_else(|| next.worktree_path.clone());
                    *row = next;
                    row.worktree_path = row.worktree_path.clone().or(worktree_path);
                }
                changed = true;
            }
            refresh_overlay_group(group);
        }
        changed
    }

    /// Apply a fused focus patch. Row `is_focused` bits mirror every listed
    /// pane. A single focused pane is a session-focus transition, so it updates
    /// the register and marks the pane viewed until the next pull.
    pub(crate) fn overlay_focus(&mut self, focused: &[PaneId], unfocused: &[PaneId]) -> bool {
        if focused.is_empty() && unfocused.is_empty() {
            return false;
        }
        let mut changed = false;
        for group in &mut self.worktree_groups {
            for row in &mut group.rows {
                let Some(pane) = row.pane.as_mut() else {
                    continue;
                };
                if focused.iter().any(|pane_id| pane_id == &pane.pane_id) {
                    changed |= !pane.is_focused;
                    pane.is_focused = true;
                }
                if unfocused.iter().any(|pane_id| pane_id == &pane.pane_id) {
                    changed |= pane.is_focused;
                    pane.is_focused = false;
                }
            }
        }
        if let [pane] = focused {
            if self.focused_pane.as_ref() != Some(pane) {
                self.focused_pane = Some(pane.clone());
                changed = true;
            }
            if !self.viewed_panes.contains(pane) {
                self.viewed_panes.push(pane.clone());
                changed = true;
            }
        } else if self
            .focused_pane
            .as_ref()
            .is_some_and(|active| unfocused.iter().any(|pane_id| pane_id == active))
        {
            self.focused_pane = None;
            changed = true;
        }
        changed
    }

    /// Fold per-agent activity heartbeats into the rollup. The agent's hook
    /// touches its heartbeat on every progress-proving event, so the freshest
    /// touch is a truer `last_activity` than the turn-grained event log — it
    /// advances per tool call, which is what keeps a busy agent's row animated,
    /// recovers an answered ask, and dates a genuine stall. Latency, not truth:
    /// a missing or older heartbeat leaves the event-log value untouched.
    ///
    /// Apply this before [`Self::with_live_panes`] so age, ranking, the
    /// ask-fold guard, and the stall window all read the accurate value.
    /// The root agent bound to this live pane, by the same stamped-id +
    /// process-start rule the sidebar's card projection binds with
    /// ([`stamped_agent_for_pane`]): a pane the multiplexer has since reused for a
    /// shell never inherits the agent that once ran there, and a pane shared by
    /// two sessions resolves to the one the card shows. The CLI's `pane list`
    /// overlay reads through this so its annotations match the rendered room
    /// rather than a looser pane-id lookup.
    pub fn agent_bound_to_pane(&self, pane: &PaneRef) -> Option<&AgentState> {
        stamped_agent_for_pane(pane, &self.agents)
    }

    pub fn with_agent_activity(mut self, activity: &[AgentActivity]) -> Self {
        for agent in &mut self.agents {
            let Some(touch) = activity
                .iter()
                .filter(|a| a.kind == agent.kind && a.agent_id == agent.agent_id)
                .max_by_key(|a| a.at)
            else {
                continue;
            };
            if touch.at > agent.last_activity {
                agent.last_activity = touch.at;
            }
        }
        self
    }
}

fn stamp_unread_rows(
    rows: &mut [crate::ledger::snapshot::row::SidebarRow],
    unread_row_ids: &BTreeSet<String>,
) {
    for row in rows {
        row.unread = unread_row_ids.contains(&row.id);
    }
}
