use super::*;
use serde::Serialize;

use rimz::ids::{AgentKind, AgentSessionId, PaneId};
use rimz::mux::ClientFocusOptions;
use rimz::pane::{PaneRef, RuntimeOwnerKind};
use rimz::store::runtime::process_owner;
use rimz::store::snapshot::{
    HookPaneRecoveryCandidate, HookPaneRecoveryContext, HookPaneRecoveryMethod,
    HookPaneRecoveryPhase,
};

pub(super) fn enrich_pane_stamp_from_cache(
    workspace: &ResolvedWorkspace,
    store: &Store,
    observation: &mut AgentLifecycleObservation,
) {
    if observation.pane_stamp.is_some() || !observation.signal.establishes_identity() {
        return;
    }
    let Some(pane_id) = observation.pane_id.as_ref() else {
        return;
    };
    let Some(frame) = rimz::sidebar::cache::read_snapshot_cache(
        &store.runtime_paths().pane_frame_path(),
        &workspace.session_name,
    ) else {
        return;
    };
    observation.pane_stamp = frame
        .to_pane_refs()
        .into_iter()
        .find(|pane| pane.pane_id == *pane_id);
}

pub(super) fn recover_focused_pane_binding(
    kind: &str,
    registers_lazily: bool,
    mux_hint: Option<MuxName>,
    workspace: &ResolvedWorkspace,
    store: &Store,
    observation: &mut AgentLifecycleObservation,
) {
    if observation.pane_id.is_some() || !registers_lazily {
        return;
    }
    if observation.parent_agent_id.is_some() {
        return;
    }
    if !matches!(
        observation.signal,
        LifecycleSignal::Registered | LifecycleSignal::TurnStarted
    ) {
        return;
    }
    let Some(agent_id) = observation
        .agent_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .map(AgentSessionId::from)
    else {
        return;
    };
    let Some(worktree_path) = observation
        .worktree_path
        .as_deref()
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned)
    else {
        return;
    };

    let kind_id = AgentKind::new_unchecked(kind);
    let mut snapshot = match store.snapshot_cached() {
        Ok(snapshot) => snapshot,
        Err(err) => {
            debug!(
                agent = kind,
                agent_id = agent_id.as_str(),
                error = %err,
                "lifecycle: skipped focused pane recovery because the prior rollup was unreadable",
            );
            return;
        }
    };
    // A provider-rested owner must not block the conversation replacing it;
    // read only the keyed sidecars that can affect this hook's pane choice.
    for agent in snapshot.agents.iter_mut().filter(|agent| {
        agent.ended_at.is_none()
            && agent.parent_agent_id.is_none()
            && agent.kind == kind_id
            && matches!(
                agent.status,
                rimz::agents::AgentStatus::Running | rimz::agents::AgentStatus::Waiting
            )
    }) {
        agent.context = rimz::store::agent_context::read_one(
            store.runtime_paths(),
            kind,
            agent.agent_id.as_str(),
        )
        .map(|record| record.context);
    }
    let phase = match observation.signal {
        LifecycleSignal::Registered => HookPaneRecoveryPhase::Registered,
        LifecycleSignal::TurnStarted => HookPaneRecoveryPhase::TurnStarted,
        _ => return,
    };
    let recovery = HookPaneRecoveryContext::new(
        &kind_id,
        &agent_id,
        observation.origin,
        phase,
        &snapshot.agents,
    );
    if recovery.already_stamped() {
        return;
    }

    let inputs = live_binding_inputs(
        mux_hint,
        &workspace.session_name,
        store.runtime_paths(),
        kind,
        agent_id.as_str(),
    );
    if inputs.panes.is_empty() {
        log_binding_recovery(
            store,
            BindingRecoveryLog::new(
                kind,
                agent_id.as_str(),
                observation,
                &worktree_path,
                BindingRecoveryOutcome::NoInputs,
            )
            .with_probes(inputs.probes),
        );
        return;
    }
    let selection = recovery.select(
        &worktree_path,
        &inputs.panes,
        inputs.client_focus.as_deref(),
    );
    let outcome = match &selection.pane_id {
        Some(pane_id) => BindingRecoveryOutcome::Selected {
            pane_id: pane_id.clone(),
            method: selection.method,
        },
        None => BindingRecoveryOutcome::Unbound {
            candidate_count: selection.candidate_count,
        },
    };
    log_binding_recovery(
        store,
        BindingRecoveryLog::new(
            kind,
            agent_id.as_str(),
            observation,
            &worktree_path,
            outcome,
        )
        .with_probes(inputs.probes)
        .with_candidates(selection.candidates.clone()),
    );
    if let Some(pane) = selection.pane {
        let pane_id = pane.pane_id.clone();
        debug!(
            agent = kind,
            agent_id = agent_id.as_str(),
            pane = %pane_id,
            "lifecycle: recovered daemon-routed pane binding from live focus",
        );
        apply_recovered_pane_binding(kind, observation, agent_id.as_str(), pane);
    } else {
        warn!(
            target: "rimz::agent::binding",
            kind,
            agent_id = agent_id.as_str(),
            cwd = worktree_path.as_str(),
            candidate_count = selection.candidate_count,
            "daemon-routed lifecycle event exhausted focused pane binding candidates",
        );
    }
}

pub(super) fn apply_recovered_pane_binding(
    kind: &str,
    observation: &mut AgentLifecycleObservation,
    agent_id: &str,
    pane: PaneRef,
) {
    apply_recovered_pane_binding_with(observation, agent_id, pane, |root_pid| {
        rimz::proc::in_pane_agent_process_for_root(kind, root_pid).map(|process| process.pid)
    });
}

/// Anchor liveness to the in-pane agent CLI, not the pane root. A shell-hosted
/// Codex pane's root pid is the host shell, which outlives the CLI and would pin
/// the card as a ghost after the agent exits. When the single-child walk can't
/// prove the CLI, leave the owner `attach_agent_owner` already set (the shared
/// daemon, reaped by the daemon-session reaper) rather than inventing one.
pub(super) fn apply_recovered_pane_binding_with(
    observation: &mut AgentLifecycleObservation,
    agent_id: &str,
    pane: PaneRef,
    resolve_owner_pid: impl Fn(u32) -> Option<u32>,
) {
    if let Some(owner_pid) = pane.pane_pid.and_then(resolve_owner_pid) {
        observation.runtime_owner = Some(process_owner(
            RuntimeOwnerKind::Agent,
            agent_id.to_owned(),
            owner_pid,
        ));
    }
    observation.pane_id = Some(pane.pane_id.clone());
    observation.pane_stamp = Some(pane);
}

fn log_binding_recovery(store: &Store, record: BindingRecoveryLog) {
    rimz::diag::binding::append(store.runtime_paths(), &record);
}

#[derive(Debug, Serialize)]
struct BindingRecoveryLog {
    event: &'static str,
    at: jiff::Timestamp,
    kind: String,
    agent_id: String,
    signal: String,
    cwd: String,
    probes: Vec<BindingProbeRecord>,
    candidates: Vec<HookPaneRecoveryCandidate>,
    outcome: BindingRecoveryOutcome,
}

impl BindingRecoveryLog {
    fn new(
        kind: &str,
        agent_id: &str,
        observation: &AgentLifecycleObservation,
        cwd: &str,
        outcome: BindingRecoveryOutcome,
    ) -> Self {
        Self {
            event: "hook_focused_pane_recovery",
            at: jiff::Timestamp::now(),
            kind: kind.to_owned(),
            agent_id: agent_id.to_owned(),
            signal: format!("{:?}", observation.signal),
            cwd: cwd.to_owned(),
            probes: Vec::new(),
            candidates: Vec::new(),
            outcome,
        }
    }

    fn with_probes(mut self, probes: Vec<BindingProbeRecord>) -> Self {
        self.probes = probes;
        self
    }

    fn with_candidates(mut self, candidates: Vec<HookPaneRecoveryCandidate>) -> Self {
        self.candidates = candidates;
        self
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
enum BindingRecoveryOutcome {
    NoInputs,
    Selected {
        pane_id: PaneId,
        method: HookPaneRecoveryMethod,
    },
    Unbound {
        candidate_count: usize,
    },
}

struct BindingInputs {
    panes: Vec<PaneRef>,
    client_focus: Option<Vec<PaneId>>,
    probes: Vec<BindingProbeRecord>,
}

#[derive(Clone, Debug, Serialize)]
struct BindingProbeRecord {
    mux: MuxName,
    pane_count: Option<usize>,
    pane_error: Option<String>,
    client_focus: Option<Vec<PaneId>>,
    focus_error: Option<String>,
}

fn live_binding_inputs(
    mux_hint: Option<MuxName>,
    session_name: &str,
    runtime: &rimz::RuntimePaths,
    kind: &str,
    agent_id: &str,
) -> BindingInputs {
    let muxes: Vec<MuxName> = mux_hint
        .map(|mux| vec![mux])
        .unwrap_or_else(|| vec![MuxName::Zellij, MuxName::Tmux]);
    let mut panes = Vec::new();
    let mut focused = Vec::new();
    let mut focus_probe_succeeded = false;
    let mut probes = Vec::new();

    for mux in muxes {
        let backend = rimz::mux::backend_for(mux);
        let mut probe = BindingProbeRecord {
            mux,
            pane_count: None,
            pane_error: None,
            client_focus: None,
            focus_error: None,
        };
        if !rimz::sidebar::produce::pane_fixture_active() {
            match session_is_live_with(session_name, || {
                backend.list_sessions_within(FOCUSED_PANE_BIND_TIMEOUT)
            }) {
                Ok(true) => {}
                Ok(false) => {
                    probe.pane_error = Some("session not live on this mux".to_owned());
                    debug!(
                        agent = kind,
                        agent_id,
                        mux = mux.as_str(),
                        session = session_name,
                        "lifecycle: focused pane recovery skipped a non-live mux session",
                    );
                    probes.push(probe);
                    continue;
                }
                Err(err) => {
                    probe.pane_error = Some(err.to_string());
                    debug!(
                        agent = kind,
                        agent_id,
                        mux = mux.as_str(),
                        session = session_name,
                        error = %err,
                        "lifecycle: focused pane recovery could not list mux sessions",
                    );
                    probes.push(probe);
                    continue;
                }
            }
        }
        match rimz::sidebar::produce::repaired_pane_frame_for_binding(
            runtime,
            mux,
            session_name,
            FOCUSED_PANE_BIND_TIMEOUT,
        ) {
            Ok(frame) => {
                let mut listed = frame.to_pane_refs();
                probe.pane_count = Some(listed.len());
                panes.append(&mut listed);
            }
            Err(err) => {
                probe.pane_error = Some(err.to_string());
                debug!(
                    agent = kind,
                    agent_id,
                    mux = mux.as_str(),
                    error = %err,
                    "lifecycle: focused pane recovery could not list panes",
                );
                probes.push(probe);
                continue;
            }
        }
        match backend
            .client_view(ClientFocusOptions {
                session_name: Some(session_name.to_owned()),
                command_timeout: Some(FOCUSED_PANE_BIND_TIMEOUT),
            })
            .map(|view| view.viewed_panes)
        {
            Ok(listed) => {
                focus_probe_succeeded = true;
                append_unique_panes(&mut focused, listed);
                probe.client_focus = Some(focused.clone());
            }
            Err(err) => {
                probe.focus_error = Some(err.to_string());
                debug!(
                    agent = kind,
                    agent_id,
                    mux = mux.as_str(),
                    error = %err,
                    "lifecycle: focused pane recovery could not read client focus",
                );
            }
        }
        probes.push(probe);
    }

    BindingInputs {
        panes,
        client_focus: focus_probe_succeeded.then_some(focused),
        probes,
    }
}

fn session_is_live_with(
    session_name: &str,
    list_sessions: impl FnOnce() -> rimz::mux::Result<Vec<String>>,
) -> rimz::mux::Result<bool> {
    list_sessions().map(|sessions| sessions.iter().any(|session| session == session_name))
}

fn append_unique_panes(target: &mut Vec<PaneId>, panes: Vec<PaneId>) {
    for pane in panes {
        if !target.iter().any(|known| known == &pane) {
            target.push(pane);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_probe_requires_an_exact_live_mux_session() {
        assert!(session_is_live_with("rimz-room", || Ok(vec!["rimz-room".to_owned()])).unwrap());
        assert!(
            !session_is_live_with("rimz-room", || Ok(vec!["rimz-room-old".to_owned()])).unwrap()
        );
        assert!(session_is_live_with("rimz-room", || Ok(Vec::new())).is_ok_and(|live| !live));
    }
}
