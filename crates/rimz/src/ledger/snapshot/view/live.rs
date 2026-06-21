use std::collections::BTreeSet;

use crate::agent_activity::AgentActivity;
use crate::agents::AgentState;
use crate::ids::PaneId;
use crate::ledger::snapshot::panes::{
    LazyAgentPairingResult, pane_admits_card, row_from_frame_pane, stamped_agent_for_pane,
};
use crate::pane::PaneRef;
use crate::schema::diag::DiagEvent;

use super::SidebarSnapshot;
use super::aggregate::{AttentionWindows, build_worktree_groups_from_rows};
use super::layout::refresh_overlay_group;
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
        self.fold_admitted_live_panes(&panes, None, None);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_live_panes_and_unread(
        mut self,
        panes: Vec<PaneRef>,
        exclude: Option<&PaneId>,
        unread_row_ids: &BTreeSet<String>,
    ) -> Self {
        let panes = Self::card_admitted_live_panes(panes, exclude);
        self.fold_admitted_live_panes(&panes, None, Some(unread_row_ids));
        self
    }

    pub(crate) fn card_admitted_live_panes(
        panes: Vec<PaneRef>,
        exclude: Option<&PaneId>,
    ) -> Vec<PaneRef> {
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
    ) -> (Self, Vec<DiagEvent>) {
        let diagnostics =
            self.fold_admitted_live_panes(&panes, Some(lazy_pairings), unread_row_ids);
        (self, diagnostics)
    }

    fn fold_admitted_live_panes(
        &mut self,
        panes: &[PaneRef],
        lazy_pairings: Option<&LazyAgentPairingResult>,
        unread_row_ids: Option<&BTreeSet<String>>,
    ) -> Vec<DiagEvent> {
        let mut projection = rows_from_panes(
            &self.agents,
            &self.needs_attention,
            &self.resolver_working,
            panes,
            LazyAgentPaneProjection {
                wired_kinds: &self.wired_lazy_kinds,
                default_models: &self.lazy_agent_default_models,
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
            &self.agents,
            self.project_root.as_deref(),
            &self.worktree_roots,
            self.root_class,
            self.now,
            AttentionWindows {
                stalled_after_secs: self.attention.stalled_after_secs.get(),
                inactive_after_secs: self.attention.inactive_after_secs.get(),
            },
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
        self.worktree_groups
            .retain(|group| !group.rows.is_empty() || group.hidden_count > 0);
        if self
            .own_view
            .as_ref()
            .and_then(|view| view.active_pane_id.as_ref())
            .is_some_and(|active| active == pane_id)
            && let Some(view) = &mut self.own_view
        {
            view.active_pane_id = None;
            view.active_pane_is_viewed = false;
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
                    &self.wired_lazy_kinds,
                    &self.lazy_agent_default_models,
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

    /// Apply a fused per-view focus patch. Row `is_focused` bits mirror the
    /// patch for every listed pane — per-view marks are session-wide truth the
    /// pull would also report — while the own-view baseline retargets only when
    /// the patch names one of this view's own working panes: a focus move in
    /// another tab is that view's mark, never this renderer's selection
    /// baseline.
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
        if let Some(view) = &mut self.own_view {
            let own_focused = focused
                .iter()
                .filter(|&pane_id| view.working_pane_ids.contains(pane_id))
                .find(|&pane_id| view.active_pane_id.as_ref() != Some(pane_id))
                .or_else(|| {
                    focused
                        .iter()
                        .find(|&pane_id| view.working_pane_ids.contains(pane_id))
                });
            if let Some(own_focused) = own_focused {
                if view.active_pane_id.as_ref() != Some(own_focused)
                    || view.own_is_active
                    || view.focus_contested
                    || !view.active_pane_is_viewed
                {
                    view.active_pane_id = Some(own_focused.clone());
                    view.active_pane_is_viewed = true;
                    view.own_is_active = false;
                    view.focus_contested = false;
                    changed = true;
                }
            } else if view
                .active_pane_id
                .as_ref()
                .is_some_and(|active| unfocused.iter().any(|pane_id| pane_id == active))
            {
                view.active_pane_id = None;
                view.active_pane_is_viewed = false;
                changed = true;
            }
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
