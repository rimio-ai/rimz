use std::collections::{BTreeMap, BTreeSet, HashMap};

use jiff::Timestamp;

use crate::agent_activity::AgentActivity;
use crate::agents::lifecycle::TurnPhase;
use crate::feed::{AgentState, AgentStatus, FeedItem, PaneRef};
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::ledger::snapshot::panes::{
    AgentPaneRow, LazyAgentPairingResult, agent_for_pane, agent_pane_for_pane,
    compute_lazy_agent_pairings, pane_admits_card, pane_start_matches, row_from_frame_pane,
};
use crate::ledger::snapshot::process::{pane_command_is_known, row_from_process};
use crate::ledger::snapshot::row::SidebarRow;

use super::SidebarSnapshot;
use super::aggregate::build_worktree_groups_from_rows;
use super::layout::refresh_overlay_group;
use super::rows::{
    active_resolver_state, agent_id_from_item, row_from_agent, row_from_standalone_item,
};

impl SidebarSnapshot {
    /// Fold live multiplexer panes into the sidebar view-model. This reducer is
    /// pure: callers own pane discovery and pass the result in, so snapshot
    /// building stays independent of any backend command.
    pub fn with_live_panes(mut self, panes: Vec<PaneRef>, exclude: Option<&PaneId>) -> Self {
        let panes = Self::card_admitted_live_panes(panes, exclude);
        self.fold_admitted_live_panes(&panes, None);
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

    pub(crate) fn with_admitted_live_panes(
        mut self,
        panes: Vec<PaneRef>,
        lazy_pairings: &LazyAgentPairingResult,
    ) -> Self {
        self.fold_admitted_live_panes(&panes, Some(lazy_pairings));
        self
    }

    fn fold_admitted_live_panes(
        &mut self,
        panes: &[PaneRef],
        lazy_pairings: Option<&LazyAgentPairingResult>,
    ) {
        self.worktree_groups = build_worktree_groups_from_rows(
            rows_from_panes(
                &self.agents,
                &self.needs_attention,
                &self.resolver_working,
                panes,
                LazyAgentPaneProjection {
                    wired_kinds: &self.wired_lazy_kinds,
                    default_models: &self.lazy_agent_default_models,
                    pairings: lazy_pairings,
                },
                self.now,
            ),
            &self.agents,
            self.project_root.as_deref(),
            &self.worktree_roots,
            self.root_class,
            self.now,
            self.sidebar.attention.stalled_after_secs.get(),
        );
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
            if let Some(own_focused) = focused
                .iter()
                .find(|&pane_id| view.working_pane_ids.contains(pane_id))
            {
                if view.active_pane_id.as_ref() != Some(own_focused) || view.own_is_active {
                    view.active_pane_id = Some(own_focused.clone());
                    view.own_is_active = false;
                    changed = true;
                }
            } else if view
                .active_pane_id
                .as_ref()
                .is_some_and(|active| unfocused.iter().any(|pane_id| pane_id == active))
            {
                view.active_pane_id = None;
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

struct LazyAgentPaneProjection<'a> {
    wired_kinds: &'a [String],
    default_models: &'a BTreeMap<String, String>,
    pairings: Option<&'a LazyAgentPairingResult>,
}

fn rows_from_panes(
    agents: &[AgentState],
    needs_attention: &[FeedItem],
    resolver_working: &[FeedItem],
    panes: &[PaneRef],
    lazy_agents: LazyAgentPaneProjection<'_>,
    now: Timestamp,
) -> Vec<SidebarRow> {
    let mut rows = Vec::new();
    let mut bound_agents: BTreeSet<(AgentKind, AgentSessionId)> = BTreeSet::new();
    let standalone_items = standalone_items_by_pane(needs_attention, resolver_working, panes);
    let computed_pairings;
    let lazy_pairings = if let Some(pairings) = lazy_agents.pairings {
        pairings
    } else {
        computed_pairings = compute_lazy_agent_pairings(panes, agents);
        &computed_pairings
    };

    for pane in panes {
        let standalone_ask = standalone_items.get(&pane.pane_id).copied();
        if let Some(agent) = agent_for_pane(pane, agents, &bound_agents) {
            push_agent_row(
                &mut rows,
                &mut bound_agents,
                agent,
                pane,
                pane_ask(agent, standalone_ask, needs_attention, resolver_working),
                now,
            );
        } else if let Some(bind) = agent_pane_for_pane(
            pane,
            agents,
            lazy_pairings,
            &bound_agents,
            lazy_agents.wired_kinds,
            lazy_agents.default_models,
            now,
        ) {
            // The cwd relaxation of stamped-id binding. A lazy-registering
            // agent (Codex) can be present without a stamped session, and a
            // non-lazy agent can lose its stamp across a mux rebirth while its
            // process keeps running. `agent_pane_for_pane` owns the whole case:
            // an unstamped session binds the live agent pane in its worktree by
            // cwd, and a wired-but-unbound lazy pane (no session yet) renders as
            // an idle agent rather than a bare process row. Remote-control and
            // app-server broker host panes are filtered out upstream
            // (`with_live_panes`), so they never reach here.
            match bind {
                AgentPaneRow::Agent(agent) => push_agent_row(
                    &mut rows,
                    &mut bound_agents,
                    agent,
                    pane,
                    pane_ask(agent, standalone_ask, needs_attention, resolver_working),
                    now,
                ),
                AgentPaneRow::Idle(row) => {
                    // The synthesized idle row is the pane's card, so a frame-
                    // admitted standalone ask folds onto it exactly as it folds
                    // onto a bound agent's row.
                    let mut row = *row;
                    if let Some(ask) = standalone_ask {
                        fold_ask_onto_row(&mut row, ask);
                    }
                    rows.push(row);
                }
            }
        } else if let Some(item) = standalone_ask {
            rows.push(row_from_standalone_item(item, pane));
        } else if pane_command_is_known(pane) {
            rows.push(row_from_process(pane, now));
        }
        // else: a brand-new or raced pane whose command is still unknown after
        // frame rotation — the third honest-read guard. Presence without
        // identity folds no row until a read names it; the pane stays in the
        // published pane frame, so the sibling count and selection baseline see
        // it.
    }

    rows
}

/// The newest pending standalone (non-agent-hook) ask per frame-admitted pane.
/// Pane-keyed because the ask's card is the pane's card: one pane renders one
/// row, so two scripts asking from one pane collapse to the newest while the
/// older stays rollup metadata until that one resolves. An ask naming no pane,
/// or a pane absent from the frame, is dropped here — no live pane, no card.
fn standalone_items_by_pane<'a>(
    needs_attention: &'a [FeedItem],
    resolver_working: &'a [FeedItem],
    panes: &[PaneRef],
) -> HashMap<PaneId, &'a FeedItem> {
    let mut by_pane = HashMap::new();
    for item in needs_attention.iter().chain(resolver_working.iter()) {
        if item.source_kind == "agent-hook" {
            continue;
        }
        let Some(pane) = frame_pane_for_item(item, panes) else {
            continue;
        };
        by_pane
            .entry(pane.pane_id.clone())
            .and_modify(|current: &mut &'a FeedItem| {
                if item.updated_at > current.updated_at {
                    *current = item;
                }
            })
            .or_insert(item);
    }
    by_pane
}

fn frame_pane_for_item<'a>(item: &FeedItem, panes: &'a [PaneRef]) -> Option<&'a PaneRef> {
    let requested = item.pane.as_ref()?;
    panes
        .iter()
        .find(|pane| pane.pane_id == requested.pane_id && pane_start_matches(requested, pane))
}

/// The single pending ask folded onto an agent's pane row. A frame-admitted
/// standalone script/bridge ask naming the pane outranks the session's own
/// agent-hook ask: it blocks the pane's foreground right now, and the agent's
/// activity never settles it — unlike a native ask it clears only when the
/// request resolves. Without one, the session's most-relevant agent-hook ask
/// stands ([`most_relevant_ask`]).
fn pane_ask<'a>(
    agent: &AgentState,
    standalone_ask: Option<&'a FeedItem>,
    needs_attention: &'a [FeedItem],
    resolver_working: &'a [FeedItem],
) -> Option<&'a FeedItem> {
    standalone_ask.or_else(|| most_relevant_ask(agent, needs_attention, resolver_working))
}

/// Render `agent` on `pane`: mark it bound, project its row, overlay the live
/// pane cwd as the worktree fallback, attach the pane, and fold the caller-
/// resolved pending ask ([`pane_ask`]) — keeping the agent's identity and
/// capability line on the row instead of swapping in a bare ask card. Shared
/// by the two binds — the stamped-id match and the Codex daemon's cwd
/// fallback — so both render identically.
fn push_agent_row(
    rows: &mut Vec<SidebarRow>,
    bound: &mut BTreeSet<(AgentKind, AgentSessionId)>,
    agent: &AgentState,
    pane: &PaneRef,
    ask: Option<&FeedItem>,
    now: Timestamp,
) {
    bound.insert((agent.kind.clone(), agent.agent_id.clone()));
    let mut row = row_from_agent(agent, now);
    row.worktree_path = row.worktree_path.or_else(|| pane.cwd.clone());
    row.pane = Some(pane.clone());
    if let Some(ask) = ask {
        fold_ask_onto_row(&mut row, ask);
    }
    rows.push(row);
}

/// The agent's single most-relevant pending ask: the newest agent-hook ask that
/// names this session and that the agent has not already moved past. Asks
/// arrive newest-first, so the first match wins. Folding only one ask onto the
/// row is the read-side guarantee that a session never stacks more than one
/// attention row.
fn most_relevant_ask<'a>(
    agent: &AgentState,
    needs_attention: &'a [FeedItem],
    resolver_working: &'a [FeedItem],
) -> Option<&'a FeedItem> {
    needs_attention
        .iter()
        .chain(resolver_working.iter())
        .find(|item| {
            item.source_kind == "agent-hook"
                && item.source == agent.kind
                && agent_id_from_item(item).as_deref() == Some(agent.agent_id.as_str())
                && !agent_moved_past_ask(agent, item)
        })
}

/// True when the agent recorded progress activity *after* raising this ask — it
/// answered in its own UI and kept working, so the ask is settled and must not
/// re-raise the row to `waiting`. This is the read-side recovery for a native_ui
/// ask the agent never reports back through Rimz: the per-tool activity
/// heartbeat advances `last_activity` past the ask's `updated_at` as soon as the
/// agent runs its next tool. A bridge ask keeps the hook blocked, so the agent
/// emits no progress while it waits and this never fires for one mid-flight.
/// Sound only because a blocked agent's `last_activity` is its *own*: a
/// backgrounded subagent keeps emitting child-stamped events while the parent
/// blocks, and the adapters drop those from the lifecycle channel
/// (`resolve_root_identity`) — folded onto the parent they would advance it
/// past a pending ask and misfire this recovery.
fn agent_moved_past_ask(agent: &AgentState, ask: &FeedItem) -> bool {
    agent.last_activity > ask.updated_at
}

/// Overlay a pending ask onto its agent's pane row: the row keeps the agent's
/// identity and capability line but takes the ask's waiting status, request,
/// surface, resolver, options, and age.
fn fold_ask_onto_row(row: &mut SidebarRow, ask: &FeedItem) {
    row.last_activity = ask.updated_at;
    let Some(agent) = row.as_agent_mut() else {
        return;
    };
    agent.status = Some(AgentStatus::Waiting);
    // Phase is a head on Running — the reduced state's invariant — so the
    // waiting overlay drops it rather than carrying a stale Reasoning/Acting.
    agent.phase = TurnPhase::Idle;
    agent.request_id = Some(ask.request_id.clone());
    agent.surface = Some(ask.surface);
    agent.resolver = active_resolver_state(ask);
    agent.options = ask.options.clone();
}
