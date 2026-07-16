use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use crate::agent_activity::AgentActivity;
use crate::agents::state::{append_recent_prompt, usable_description};
use crate::agents::{
    AgentState, AgentStatus, LocalSessionObservation, LocalSessionProjection, LocalSessionState,
    ProviderCapacity, TurnPhase,
};
use crate::diag::record::DiagEvent;
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::pane::PaneRef;
use crate::store::snapshot::panes::{
    LazyAgentPairingResult, PaneBindingIndex, pane_admits_card, row_from_frame_pane,
    stamped_agent_for_pane,
};

use super::SidebarSnapshot;
use super::aggregate::{AgentProjection, AttentionWindows, build_worktree_groups_from_rows};
use super::layout::{GroupRoots, refresh_overlay_group};
use projection::{LazyAgentPaneProjection, rows_from_panes};

mod projection;

#[cfg(test)]
pub(crate) use projection::row_identity_violations;

impl SidebarSnapshot {
    /// Merge provider-owned local sessions only after strict one-to-one binding
    /// to the current pane incarnation. These agents are transient snapshot
    /// rows: the durable rollup and event log remain untouched.
    #[doc(hidden)]
    pub fn with_local_sessions(
        mut self,
        panes: &[PaneRef],
        mut observations: Vec<LocalSessionObservation>,
    ) -> Self {
        observations.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then(left.session_id.cmp(&right.session_id))
        });
        let mut used_panes = HashSet::new();
        let mut used_sessions = BTreeSet::new();
        let mut bindings = Vec::new();
        let binding_index = PaneBindingIndex::new(&self.agents);

        for (observation_index, observation) in observations.iter().enumerate() {
            // A hook-bound session already carries stronger identity than a
            // provider's workspace-latest cache: the durable row stamped this
            // exact live pane with the same `(kind, session id)`. Use that
            // authority before command-line resume discovery, then leave the
            // fresh-cache path below for sessions that registered no hook.
            let stamped = panes.iter().find(|pane| {
                binding_index.stamped_agent(pane).is_some_and(|agent| {
                    agent.kind == observation.kind
                        && agent.agent_id == observation.session_id
                        && local_pane_matches(pane, observation)
                        && !used_panes.contains(&pane.pane_id)
                })
            });
            if let Some(pane) = stamped {
                // Exact durable identity consumes both sides before lifecycle
                // freshness is considered, so a stale provider fold cannot
                // later bind this pane to another same-cwd session.
                used_sessions.insert(observation_index);
                used_panes.insert(pane.pane_id.clone());
                bindings.push((observation_index, pane.clone()));
                continue;
            }
            let Some(pane) = panes.iter().find(|pane| {
                pane.resumed_session_id.as_ref() == Some(&observation.session_id)
                    && local_pane_matches(pane, observation)
                    && !used_panes.contains(&pane.pane_id)
            }) else {
                continue;
            };
            used_panes.insert(pane.pane_id.clone());
            used_sessions.insert(observation_index);
            bindings.push((observation_index, pane.clone()));
        }
        drop(binding_index);

        let mut fresh = observations
            .iter()
            .enumerate()
            .filter_map(|(index, observation)| {
                if used_sessions.contains(&index) {
                    return None;
                }
                Some((index, observation, observation.fresh_binding_at?))
            })
            .collect::<Vec<_>>();
        fresh.sort_by(
            |(left_index, left, left_at), (right_index, right, right_at)| {
                right_at
                    .cmp(left_at)
                    .then(right.created_at.cmp(&left.created_at))
                    .then(right.session_id.cmp(&left.session_id))
                    .then(right_index.cmp(left_index))
            },
        );
        for (observation_index, observation, fresh_binding_at) in fresh {
            let viable = panes
                .iter()
                .filter(|pane| !used_panes.contains(&pane.pane_id))
                .filter(|pane| pane.resumed_session_id.is_none())
                .filter(|pane| local_pane_matches(pane, observation))
                .filter(|pane| match pane.pane_process_start {
                    Some(start) => start <= fresh_binding_at && observation.last_activity >= start,
                    None => observation.first_event_at.is_some(),
                })
                .collect::<Vec<_>>();
            let Some(pane) = unique_closest_pane(&viable) else {
                continue;
            };
            used_panes.insert(pane.pane_id.clone());
            bindings.push((observation_index, pane.clone()));
        }

        for (observation_index, pane) in bindings {
            merge_bound_local_session(&mut self.agents, &observations[observation_index], &pane);
        }
        self
    }

    /// Fold live multiplexer panes into the sidebar view-model. This reducer is
    /// pure: callers own pane discovery and pass the result in, so snapshot
    /// building stays independent of any backend command.
    pub fn with_live_panes(mut self, panes: Vec<PaneRef>, exclude: Option<&PaneId>) -> Self {
        let panes = Self::card_admitted_live_panes(panes, exclude);
        self.fold_admitted_live_panes(&panes, None, None, &BTreeMap::new(), &BTreeSet::new());
        self
    }

    #[cfg(test)]
    pub(crate) fn with_live_panes_and_provider_capacities(
        mut self,
        panes: Vec<PaneRef>,
        exclude: Option<&PaneId>,
        provider_capacities: &BTreeMap<AgentKind, ProviderCapacity>,
    ) -> Self {
        let panes = Self::card_admitted_live_panes(panes, exclude);
        self.fold_admitted_live_panes(&panes, None, None, provider_capacities, &BTreeSet::new());
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
        provider_capacities: &BTreeMap<AgentKind, ProviderCapacity>,
        exhausted_resumes: &BTreeSet<(AgentKind, AgentSessionId)>,
    ) -> (Self, Vec<DiagEvent>) {
        let diagnostics = self.fold_admitted_live_panes(
            &panes,
            Some(lazy_pairings),
            unread_row_ids,
            provider_capacities,
            exhausted_resumes,
        );
        (self, diagnostics)
    }

    fn fold_admitted_live_panes(
        &mut self,
        panes: &[PaneRef],
        lazy_pairings: Option<&LazyAgentPairingResult>,
        unread_row_ids: Option<&BTreeSet<String>>,
        provider_capacities: &BTreeMap<AgentKind, ProviderCapacity>,
        exhausted_resumes: &BTreeSet<(AgentKind, AgentSessionId)>,
    ) -> Vec<DiagEvent> {
        let mut projection = rows_from_panes(
            &self.agents,
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
                provider_capacities,
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

fn local_pane_matches(pane: &PaneRef, observation: &LocalSessionObservation) -> bool {
    crate::store::snapshot::process::pane_agent_kind(pane) == Some(observation.kind.as_str())
        && crate::store::snapshot::process::pane_worktree_path(pane).is_some_and(|workspace| {
            crate::worktree::normalize_path_lexical(Path::new(workspace)) == observation.workspace
        })
}

fn local_observation_is_current(agent: &AgentState, observation: &LocalSessionObservation) -> bool {
    matches!(observation.projection, LocalSessionProjection::Lifecycle(_))
        && observation.last_activity.as_second() >= agent.last_activity.as_second()
}

fn unique_closest_pane<'a>(viable: &[&'a PaneRef]) -> Option<&'a PaneRef> {
    if let [pane] = viable {
        return Some(*pane);
    }
    let newest_start = viable
        .iter()
        .filter_map(|pane| pane.pane_process_start)
        .max()?;
    let mut newest = viable
        .iter()
        .copied()
        .filter(|pane| pane.pane_process_start == Some(newest_start));
    let selected = newest.next()?;
    newest.next().is_none().then_some(selected)
}

fn merge_bound_local_session(
    agents: &mut Vec<AgentState>,
    observation: &LocalSessionObservation,
    pane: &PaneRef,
) {
    let exact_index = agents.iter().position(|agent| {
        agent.kind == observation.kind && agent.agent_id == observation.session_id
    });
    let prior_index = exact_index.or_else(|| {
        agents.iter().position(|agent| {
            agent.kind == observation.kind
                && agent.agent_id.is_provisional()
                && agent
                    .pane
                    .as_ref()
                    .is_some_and(|stamped| stamped.pane_id == pane.pane_id)
        })
    });
    let prior = prior_index.map(|index| agents.remove(index));
    agents.retain(|agent| {
        !(agent.kind == observation.kind && agent.agent_id == observation.session_id)
    });
    let mut state = prior.unwrap_or_else(|| empty_local_agent(observation));
    state.agent_id = observation.session_id.clone();
    state.kind = observation.kind.clone();
    state.pane = Some(pane.clone());
    state.worktree_path = Some(observation.workspace.to_string_lossy().into_owned());
    state.transcript_path = Some(observation.transcript_path.to_string_lossy().into_owned());

    if exact_index.is_some() {
        if local_observation_is_current(&state, observation)
            && let LocalSessionProjection::Lifecycle(projection) = &observation.projection
        {
            apply_local_lifecycle(&mut state, observation, projection);
        }
    } else {
        state.parent_agent_id = None;
        match &observation.projection {
            LocalSessionProjection::IdentityOnly => {
                state.status = AgentStatus::Idle;
                state.phase = TurnPhase::Idle;
                state.task = None;
                state.prompt = None;
                state.recent_prompts.clear();
                state.context_pct = None;
                state.turn_started_at = None;
                state.waiting_since = None;
                state.open_ask = None;
                state.compacting_since = None;
                state.last_seen = observation.last_activity;
                state.last_activity = observation.last_activity;
            }
            LocalSessionProjection::Lifecycle(projection) => {
                state.turn_started_at = observation.first_event_at;
                apply_local_lifecycle(&mut state, observation, projection);
            }
        }
        state.registered_at = Some(observation.created_at);
    }
    agents.push(state);
}

fn apply_local_lifecycle(
    state: &mut AgentState,
    observation: &LocalSessionObservation,
    projection: &LocalSessionState,
) {
    state.status = projection.status;
    state.phase = projection.phase;
    state.task = projection.native_prompt_detail.clone();
    if let Some(prompt) = projection.latest_prompt.as_deref() {
        if state.first_prompt.is_none() && usable_description(prompt) {
            state.first_prompt = Some(prompt.to_owned());
        }
        state.prompt = Some(prompt.to_owned());
        append_recent_prompt(&mut state.recent_prompts, prompt);
    }
    state.context_pct = projection.context_pct;
    state.waiting_since = projection.waiting_since;
    // Provider-native approvals are observable but remain pane-only.
    state.open_ask = None;
    state.last_seen = observation.last_activity;
    state.last_activity = observation.last_activity;
}

fn empty_local_agent(observation: &LocalSessionObservation) -> AgentState {
    let status = match &observation.projection {
        LocalSessionProjection::IdentityOnly => AgentStatus::Idle,
        LocalSessionProjection::Lifecycle(projection) => projection.status,
    };
    let mut state = AgentState::seed(
        observation.kind.clone(),
        observation.session_id.clone(),
        status,
        observation.last_activity,
    );
    state.registered_at = Some(observation.created_at);
    state
}

fn stamp_unread_rows(
    rows: &mut [crate::store::snapshot::row::SidebarRow],
    unread_row_ids: &BTreeSet<String>,
) {
    for row in rows {
        row.unread = unread_row_ids.contains(&row.id);
    }
}
