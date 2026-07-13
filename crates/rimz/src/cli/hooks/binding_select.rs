use super::*;

pub(super) struct PriorAgentPane<'a> {
    pub(super) kind: &'a str,
    pub(super) agent_id: &'a str,
    pub(super) pane_id: Option<&'a PaneId>,
    pub(super) status: rimz::agents::AgentStatus,
    pub(super) origin: Option<rimz::agents::SessionOrigin>,
    /// The session's last recorded activity — what decides whether its stamp
    /// still plausibly owns a live pane ([`pane_start_allows_bind`]).
    pub(super) last_activity: jiff::Timestamp,
}

pub(super) fn prior_agent_panes(agents: &[AgentState]) -> Vec<PriorAgentPane<'_>> {
    agents
        .iter()
        .map(|agent| PriorAgentPane {
            kind: agent.kind.as_str(),
            agent_id: agent.agent_id.as_str(),
            pane_id: agent.pane.as_ref().map(|pane| &pane.pane_id),
            status: agent.status,
            origin: agent.origin,
            last_activity: agent.last_activity,
        })
        .collect()
}

#[derive(Clone, Copy)]
pub(super) struct BindingSession<'a> {
    pub(super) kind: &'a str,
    pub(super) agent_id: &'a str,
    pub(super) origin: Option<rimz::agents::SessionOrigin>,
    pub(super) registered_at: Option<jiff::Timestamp>,
}

pub(super) fn select_focused_pane_binding(
    incoming: BindingSession<'_>,
    worktree_path: &str,
    prior_agents: &[PriorAgentPane<'_>],
    panes: &[PaneRef],
    client_focus: Option<&[PaneId]>,
    allow_occupied_daemon_pane: bool,
) -> FocusedPaneBindingSelection {
    let mut candidate_records = panes
        .iter()
        .map(|pane| {
            binding_candidate_record(
                incoming.kind,
                incoming.agent_id,
                worktree_path,
                incoming.registered_at,
                prior_agents,
                pane,
            )
        })
        .collect::<Vec<_>>();
    let mut candidates = selectable_binding_candidates(panes, &candidate_records, false);
    let mut occupied_sole_candidate = false;
    let mut occupied_candidate_count = None;
    if candidates.is_empty()
        && allow_occupied_daemon_pane
        && daemon_session_can_share_occupied_pane(incoming.kind, incoming.agent_id, prior_agents)
    {
        candidates = selectable_occupied_daemon_candidates(panes, &candidate_records, client_focus);
        if candidates.is_empty() && incoming.origin == Some(rimz::agents::SessionOrigin::Fresh) {
            candidates = selectable_resting_fresh_occupied_candidates(
                panes,
                &candidate_records,
                incoming.kind,
                prior_agents,
            );
            occupied_sole_candidate = candidates.len() == 1;
            if !occupied_sole_candidate {
                occupied_candidate_count = Some(candidates.len());
                candidates.clear();
            }
        }
        allow_occupied_daemon_candidates(&mut candidate_records, &candidates);
    }
    let candidate_count = occupied_candidate_count.unwrap_or(candidates.len());
    if candidates.is_empty() {
        return FocusedPaneBindingSelection {
            pane: None,
            pane_id: None,
            candidate_count,
            method: BindingSelectionMethod::None,
            candidates: candidate_records,
        };
    }

    if candidates.len() == 1 {
        return FocusedPaneBindingSelection {
            pane: Some(candidates[0].clone()),
            pane_id: Some(candidates[0].pane_id.clone()),
            candidate_count,
            method: if occupied_sole_candidate {
                BindingSelectionMethod::OccupiedSoleCandidate
            } else {
                BindingSelectionMethod::SingleCandidate
            },
            candidates: candidate_records,
        };
    }

    let (pane, method) = if let Some(client_focus) = client_focus {
        let focused_panes: Vec<&PaneRef> = candidates
            .iter()
            .copied()
            .filter(|pane| client_focus.iter().any(|focused| focused == &pane.pane_id))
            .collect();
        annotate_focus_rejections(
            &mut candidate_records,
            &candidates,
            &focused_panes,
            BindingRejectReason::NotInClientFocus,
        );
        let selected = unique_pane(focused_panes.iter().copied());
        if selected.is_none() && focused_panes.len() > 1 {
            annotate_ambiguous(&mut candidate_records, &focused_panes);
        }
        (selected, BindingSelectionMethod::ClientFocus)
    } else {
        let focused_panes: Vec<&PaneRef> = candidates
            .iter()
            .copied()
            .filter(|pane| pane.is_focused)
            .collect();
        annotate_focus_rejections(
            &mut candidate_records,
            &candidates,
            &focused_panes,
            BindingRejectReason::NotTabFocused,
        );
        let selected = unique_pane(focused_panes.iter().copied());
        if selected.is_none() && focused_panes.len() > 1 {
            annotate_ambiguous(&mut candidate_records, &focused_panes);
        }
        (selected, BindingSelectionMethod::TabFocus)
    };
    let pane_id = pane.as_ref().map(|pane| pane.pane_id.clone());
    FocusedPaneBindingSelection {
        pane,
        pane_id,
        candidate_count,
        method,
        candidates: candidate_records,
    }
}

fn selectable_resting_fresh_occupied_candidates<'a>(
    panes: &'a [PaneRef],
    records: &[BindingCandidateRecord],
    kind: &str,
    prior_agents: &[PriorAgentPane<'_>],
) -> Vec<&'a PaneRef> {
    panes
        .iter()
        .zip(records.iter())
        .filter_map(|(pane, record)| {
            let owner_id = record.occupied_by_agent_id.as_deref()?;
            let owner = prior_agents
                .iter()
                .find(|agent| agent.kind == kind && agent.agent_id == owner_id)?;
            (binding_candidate_selectable(record, true)
                && !matches!(
                    owner.status,
                    rimz::agents::AgentStatus::Running | rimz::agents::AgentStatus::Waiting
                )
                && owner.origin == Some(rimz::agents::SessionOrigin::Fresh))
            .then_some(pane)
        })
        .collect()
}

fn selectable_occupied_daemon_candidates<'a>(
    panes: &'a [PaneRef],
    records: &[BindingCandidateRecord],
    client_focus: Option<&[PaneId]>,
) -> Vec<&'a PaneRef> {
    panes
        .iter()
        .zip(records.iter())
        .filter_map(|(pane, record)| {
            (binding_candidate_selectable(record, true)
                && pane_is_focus_evidence(pane, client_focus))
            .then_some(pane)
        })
        .collect()
}

fn pane_is_focus_evidence(pane: &PaneRef, client_focus: Option<&[PaneId]>) -> bool {
    client_focus
        .map(|focused| focused.iter().any(|pane_id| pane_id == &pane.pane_id))
        .unwrap_or(pane.is_focused)
}

fn selectable_binding_candidates<'a>(
    panes: &'a [PaneRef],
    records: &[BindingCandidateRecord],
    allow_occupied_daemon_pane: bool,
) -> Vec<&'a PaneRef> {
    panes
        .iter()
        .zip(records.iter())
        .filter_map(|(pane, record)| {
            binding_candidate_selectable(record, allow_occupied_daemon_pane).then_some(pane)
        })
        .collect()
}

fn binding_candidate_selectable(
    record: &BindingCandidateRecord,
    allow_occupied_daemon_pane: bool,
) -> bool {
    record.reject_reasons.is_empty()
        || allow_occupied_daemon_pane
            && record
                .reject_reasons
                .iter()
                .all(|reason| matches!(reason, BindingRejectReason::StampedToOther { .. }))
}

fn daemon_session_can_share_occupied_pane(
    kind: &str,
    agent_id: &str,
    prior_agents: &[PriorAgentPane<'_>],
) -> bool {
    rimz::agents::descriptor_by_kind(kind)
        .is_some_and(|descriptor| descriptor.capabilities.daemon_hooked_sessions)
        && !prior_agents
            .iter()
            .any(|agent| agent.kind == kind && agent.agent_id == agent_id)
}

fn allow_occupied_daemon_candidates(
    records: &mut [BindingCandidateRecord],
    candidates: &[&PaneRef],
) {
    for candidate in candidates {
        let Some(record) = records
            .iter_mut()
            .find(|record| record.pane_id == candidate.pane_id)
        else {
            continue;
        };
        record
            .reject_reasons
            .retain(|reason| !matches!(reason, BindingRejectReason::StampedToOther { .. }));
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FocusedPaneBindingSelection {
    pub(super) pane: Option<PaneRef>,
    pub(super) pane_id: Option<PaneId>,
    pub(super) candidate_count: usize,
    pub(super) method: BindingSelectionMethod,
    pub(super) candidates: Vec<BindingCandidateRecord>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum BindingSelectionMethod {
    None,
    SingleCandidate,
    OccupiedSoleCandidate,
    ClientFocus,
    TabFocus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct BindingCandidateRecord {
    pub(super) pane_id: PaneId,
    pub(super) cwd: Option<String>,
    pub(super) command: Option<String>,
    pub(super) is_focused: bool,
    pub(super) pane_process_start: Option<jiff::Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) occupied_by_agent_id: Option<String>,
    pub(super) reject_reasons: Vec<BindingRejectReason>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub(super) enum BindingRejectReason {
    CwdMismatch { got: Option<String> },
    CommandMismatch { got: Option<String> },
    StampedToOther { agent_id: String },
    StartedAfterSession,
    NotInClientFocus,
    NotTabFocused,
    Ambiguous { n: usize },
}

fn binding_candidate_record(
    kind: &str,
    agent_id: &str,
    worktree_path: &str,
    incoming_registered_at: Option<jiff::Timestamp>,
    prior_agents: &[PriorAgentPane<'_>],
    pane: &PaneRef,
) -> BindingCandidateRecord {
    let mut reject_reasons = Vec::new();
    if pane.cwd.as_deref() != Some(worktree_path) {
        reject_reasons.push(BindingRejectReason::CwdMismatch {
            got: pane.cwd.clone(),
        });
    }
    if rimz::store::snapshot::pane_agent_kind(pane) != Some(kind) {
        reject_reasons.push(BindingRejectReason::CommandMismatch {
            got: pane.command.clone(),
        });
    }
    let occupied_by_agent_id =
        pane_stamped_to_other_agent(kind, agent_id, prior_agents, pane).map(ToOwned::to_owned);
    if let Some(stamped) = &occupied_by_agent_id {
        reject_reasons.push(BindingRejectReason::StampedToOther {
            agent_id: stamped.clone(),
        });
    }
    if incoming_registered_at.is_some_and(|registered_at| {
        pane.pane_process_start
            .is_some_and(|started_at| started_at > registered_at)
            && pane
                .resumed_session_id
                .as_ref()
                .is_none_or(|resumed| resumed.as_str() != agent_id)
    }) {
        reject_reasons.push(BindingRejectReason::StartedAfterSession);
    }
    BindingCandidateRecord {
        pane_id: pane.pane_id.clone(),
        cwd: pane.cwd.clone(),
        command: pane.command.clone(),
        is_focused: pane.is_focused,
        pane_process_start: pane.pane_process_start,
        occupied_by_agent_id,
        reject_reasons,
    }
}

fn annotate_focus_rejections(
    records: &mut [BindingCandidateRecord],
    candidates: &[&PaneRef],
    focused_panes: &[&PaneRef],
    reason: BindingRejectReason,
) {
    for pane in candidates {
        if focused_panes
            .iter()
            .any(|focused| focused.pane_id == pane.pane_id)
        {
            continue;
        }
        if let Some(record) = records
            .iter_mut()
            .find(|record| record.pane_id == pane.pane_id)
        {
            record.reject_reasons.push(reason.clone());
        }
    }
}

fn annotate_ambiguous(records: &mut [BindingCandidateRecord], focused_panes: &[&PaneRef]) {
    for pane in focused_panes {
        if let Some(record) = records
            .iter_mut()
            .find(|record| record.pane_id == pane.pane_id)
        {
            record.reject_reasons.push(BindingRejectReason::Ambiguous {
                n: focused_panes.len(),
            });
        }
    }
}

/// Whether another session's durable stamp still plausibly owns this pane. A
/// stamp is only a pane id and a mux rebirth reuses ids, so a stamp whose
/// session's last activity predates the pane's current process start is a prior
/// tenant's residue: projection refuses such a bind ([`pane_start_allows_bind`]),
/// and it must not block recovery either — or a reborn pane id could never be
/// stamped again. A pane with no readable process start keeps the conservative
/// block: an unprovable stamp is treated as live.
fn pane_stamped_to_other_agent<'a>(
    kind: &str,
    agent_id: &str,
    prior_agents: &'a [PriorAgentPane<'_>],
    pane: &PaneRef,
) -> Option<&'a str> {
    if let Some(agent) = prior_agents.iter().find(|agent| {
        agent.kind == kind
            && agent.agent_id != agent_id
            && agent.pane_id.is_some_and(|known| known == &pane.pane_id)
            && pane_start_allows_bind(agent.last_activity, pane)
    }) {
        // Routine in a shared worktree — the sibling's pane is simply taken —
        // so this traces at debug; the anomaly signal is the caller's
        // exhausted-candidates warn.
        debug!(
            target: "rimz::agent::binding",
            kind,
            agent_id,
            stamped_agent_id = agent.agent_id,
            pane = %pane.pane_id,
            "pane stamp belongs to another live agent; skipping focused binding candidate",
        );
        Some(agent.agent_id)
    } else {
        None
    }
}

/// Whether this session already holds a durable pane stamp — the cheap early-out
/// that keeps every later turn of a stamped daemon session off the mux probes.
/// Deliberately stamp-id-only: proving a session's *own* stamp stale needs the
/// live pane list this early-out exists to avoid fetching, so a session resumed
/// into a different pane keeps its old stamp until a hook env or a fresh
/// recovery re-stamps it. The bind-side process-start guard keeps that stale
/// stamp from capturing a reused pane while the session rests
/// ([`pane_start_allows_bind`]).
pub(super) fn session_already_stamped(
    kind: &str,
    agent_id: &str,
    prior_agents: &[PriorAgentPane<'_>],
) -> bool {
    prior_agents
        .iter()
        .any(|agent| agent.kind == kind && agent.agent_id == agent_id && agent.pane_id.is_some())
}

fn unique_pane<'a>(mut panes: impl Iterator<Item = &'a PaneRef>) -> Option<PaneRef> {
    let first = panes.next()?;
    panes
        .all(|pane| pane.pane_id == first.pane_id)
        .then(|| first.clone())
}
