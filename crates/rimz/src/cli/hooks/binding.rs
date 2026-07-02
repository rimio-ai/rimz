use super::binding_select::{
    BindingCandidateRecord, BindingSelectionMethod, prior_agent_panes, select_focused_pane_binding,
    session_already_stamped,
};
use super::*;

pub(super) fn recover_focused_pane_binding(
    kind: &str,
    registers_lazily: bool,
    mux_hint: Option<MuxName>,
    workspace: &ResolvedWorkspace,
    ledger: &Ledger,
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
    let Some(agent_id) = observation.agent_id.as_deref().filter(|id| !id.is_empty()) else {
        return;
    };
    let Some(worktree_path) = observation
        .worktree_path
        .as_deref()
        .filter(|path| !path.is_empty())
    else {
        return;
    };

    let snapshot = match ledger.snapshot_cached() {
        Ok(snapshot) => snapshot,
        Err(err) => {
            debug!(
                agent = kind,
                agent_id,
                error = %err,
                "lifecycle: skipped focused pane recovery because the prior rollup was unreadable",
            );
            return;
        }
    };
    let prior = prior_agent_panes(&snapshot.agents);
    if session_already_stamped(kind, agent_id, &prior) {
        return;
    }

    let Some(inputs) = live_binding_inputs(
        mux_hint,
        &workspace.session_name,
        ledger.runtime_paths(),
        kind,
        agent_id,
    ) else {
        log_binding_recovery(
            ledger,
            BindingRecoveryLog::new(
                kind,
                agent_id,
                observation,
                worktree_path,
                BindingRecoveryOutcome::NoInputs,
            ),
        );
        return;
    };
    let selection = select_focused_pane_binding(
        kind,
        agent_id,
        worktree_path,
        &prior,
        &inputs.panes,
        inputs.client_focus.as_deref(),
        matches!(observation.signal, LifecycleSignal::TurnStarted),
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
        ledger,
        BindingRecoveryLog::new(kind, agent_id, observation, worktree_path, outcome)
            .with_probes(inputs.probes)
            .with_candidates(selection.candidates.clone()),
    );
    if let Some(pane_id) = selection.pane_id {
        debug!(
            agent = kind,
            agent_id,
            pane = %pane_id,
            "lifecycle: recovered daemon-routed pane binding from live focus",
        );
        observation.pane_id = Some(pane_id);
    } else {
        warn!(
            target: "rimz::agent::binding",
            kind,
            agent_id,
            cwd = worktree_path,
            candidate_count = selection.candidate_count,
            "daemon-routed lifecycle event exhausted focused pane binding candidates",
        );
    }
}

fn log_binding_recovery(ledger: &Ledger, record: BindingRecoveryLog) {
    rimz::diag::binding::append(ledger.runtime_paths(), &record);
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
    candidates: Vec<BindingCandidateRecord>,
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

    fn with_candidates(mut self, candidates: Vec<BindingCandidateRecord>) -> Self {
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
        method: BindingSelectionMethod,
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
) -> Option<BindingInputs> {
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

    if panes.is_empty() {
        return None;
    }
    Some(BindingInputs {
        panes,
        client_focus: focus_probe_succeeded.then_some(focused),
        probes,
    })
}

fn append_unique_panes(target: &mut Vec<PaneId>, panes: Vec<PaneId>) {
    for pane in panes {
        if !target.iter().any(|known| known == &pane) {
            target.push(pane);
        }
    }
}
