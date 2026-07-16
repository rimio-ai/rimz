//! Plugin-author conformance checks over manifests, probes, and envelope replays.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::PluginAdapter;
use super::load::load_from_root;
use super::probes::{self, ProbeCheck};
use super::protocol::{CanonicalEvent, Envelope};
use crate::agents::{
    AgentAdapter, AgentStatus, ConcernCoverage, HookCoverage, LifecycleSignal, LifecycleState,
    RootIdentity, TurnPhase, resolve_root_identity, step,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PluginCheckSummary {
    pub primary: usize,
    pub partial: usize,
    pub absent: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeCheckStatus {
    Passed(String),
    Skipped(String),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeCheckReport {
    pub name: &'static str,
    pub command: String,
    pub present: bool,
    pub executable: bool,
    pub status: ProbeCheckStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayRow {
    pub line: usize,
    pub event: String,
    pub signal: String,
    pub state: String,
    pub warning: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayFinalState {
    pub agent_id: String,
    pub status: AgentStatus,
    pub phase: TurnPhase,
    pub compacting: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayCheckReport {
    pub path: PathBuf,
    pub rows: Vec<ReplayRow>,
    pub final_states: Vec<ReplayFinalState>,
    pub rejected: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginCheckReport {
    pub kind: String,
    pub manifest_path: PathBuf,
    pub coverage: PluginCheckSummary,
    pub lifecycle: PluginCheckSummary,
    pub probes: Vec<ProbeCheckReport>,
    pub replay: Option<ReplayCheckReport>,
}

impl PluginCheckReport {
    pub fn passed(&self) -> bool {
        self.probes
            .iter()
            .all(|probe| !matches!(probe.status, ProbeCheckStatus::Failed(_)))
            && self
                .replay
                .as_ref()
                .is_none_or(|replay| replay.rejected == 0)
    }
}

pub fn check_from_root(
    root: &Path,
    kind: &str,
    spend_file: Option<&Path>,
    replay_file: Option<&Path>,
) -> Result<PluginCheckReport, String> {
    if super::super::ADAPTERS
        .iter()
        .any(|adapter| adapter.descriptor().kind == kind)
    {
        return Err(format!(
            "agent kind `{kind}` is built-in — nothing to check"
        ));
    }

    let loaded = load_from_root(root);
    if let Some(error) = loaded
        .errors
        .iter()
        .find(|error| error.kind_hint.as_deref() == Some(kind))
    {
        return Err(format!("manifest invalid: {error}"));
    }
    let adapter = loaded
        .plugin_adapters
        .iter()
        .copied()
        .find(|adapter| adapter.descriptor().kind == kind)
        .ok_or_else(|| {
            format!(
                "agent plugin `{kind}` was not found at {}",
                root.join(kind).join("agent.toml").display()
            )
        })?;
    let diagnostic = loaded
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.valid && diagnostic.kind == kind)
        .ok_or_else(|| format!("agent plugin `{kind}` has no manifest diagnostic"))?;

    let descriptor = adapter.descriptor();
    let coverage = descriptor.coverage.iter().fold(
        PluginCheckSummary::default(),
        |mut summary, (_, coverage)| {
            match coverage {
                ConcernCoverage::Wired { .. } => summary.primary += 1,
                ConcernCoverage::Partial { .. } => summary.partial += 1,
                ConcernCoverage::Unsupported { .. } => summary.absent += 1,
            }
            summary
        },
    );
    let lifecycle = descriptor.lifecycle_hooks.iter().fold(
        PluginCheckSummary::default(),
        |mut summary, (_, coverage)| {
            match coverage {
                HookCoverage::Native { .. } => summary.primary += 1,
                HookCoverage::Derived { .. } => summary.partial += 1,
                HookCoverage::Absent { .. } => summary.absent += 1,
            }
            summary
        },
    );
    let probes = check_probes(adapter, diagnostic, spend_file);
    let replay = replay_file.map(|path| replay(adapter, path)).transpose()?;

    Ok(PluginCheckReport {
        kind: kind.to_owned(),
        manifest_path: diagnostic.path.clone(),
        coverage,
        lifecycle,
        probes,
        replay,
    })
}

fn check_probes(
    adapter: &PluginAdapter,
    diagnostic: &super::PluginDiagnostic,
    spend_file: Option<&Path>,
) -> Vec<ProbeCheckReport> {
    diagnostic
        .probes
        .iter()
        .map(|probe| {
            let status = if !probe.present {
                ProbeCheckStatus::Failed("executable is missing".into())
            } else if !probe.executable {
                ProbeCheckStatus::Failed("file is not executable".into())
            } else {
                match probe.name {
                    "spend" => match spend_file {
                        Some(path) if !path.is_file() => ProbeCheckStatus::Failed(format!(
                            "spend file does not exist: {}",
                            path.display()
                        )),
                        Some(path) => adapter
                            .manifest
                            .probes
                            .spend
                            .as_ref()
                            .map(|argv| {
                                probes::check_spend(
                                    adapter.descriptor.kind,
                                    adapter.plugin_dir,
                                    argv,
                                    path,
                                )
                            })
                            .map(probe_status)
                            .unwrap_or_else(|| {
                                ProbeCheckStatus::Failed("probe declaration disappeared".into())
                            }),
                        None => {
                            ProbeCheckStatus::Skipped("pass --spend-file <path> to dry-run".into())
                        }
                    },
                    "account" => adapter
                        .manifest
                        .probes
                        .account
                        .as_ref()
                        .map(|argv| {
                            probes::check_account(adapter.descriptor.kind, adapter.plugin_dir, argv)
                        })
                        .map(probe_status)
                        .unwrap_or_else(|| {
                            ProbeCheckStatus::Failed("probe declaration disappeared".into())
                        }),
                    "version" => adapter
                        .manifest
                        .probes
                        .version
                        .as_ref()
                        .map(|argv| {
                            probes::check_version(adapter.descriptor.kind, adapter.plugin_dir, argv)
                        })
                        .map(probe_status)
                        .unwrap_or_else(|| {
                            ProbeCheckStatus::Failed("probe declaration disappeared".into())
                        }),
                    name => ProbeCheckStatus::Failed(format!("unknown probe `{name}`")),
                }
            };
            ProbeCheckReport {
                name: probe.name,
                command: probe.command.clone(),
                present: probe.present,
                executable: probe.executable,
                status,
            }
        })
        .collect()
}

fn probe_status(check: ProbeCheck) -> ProbeCheckStatus {
    match check {
        ProbeCheck::Passed(detail) => ProbeCheckStatus::Passed(detail),
        ProbeCheck::Failed(error) => ProbeCheckStatus::Failed(error),
    }
}

fn replay(adapter: &PluginAdapter, path: &Path) -> Result<ReplayCheckReport, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("cannot read replay {}: {error}", path.display()))?;
    let mut states = BTreeMap::<String, LifecycleState>::new();
    let mut rows = Vec::new();
    let mut rejected = 0;
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        if line.trim().is_empty() {
            continue;
        }
        let payload: Value = match serde_json::from_str(line) {
            Ok(payload) => payload,
            Err(error) => {
                rejected += 1;
                rows.push(rejected_row(
                    line_number,
                    "-",
                    format!("invalid JSON: {error}"),
                ));
                continue;
            }
        };
        let Some(event_name) = payload.get("hook_event_name").and_then(Value::as_str) else {
            rejected += 1;
            rows.push(rejected_row(
                line_number,
                "-",
                "missing string hook_event_name".into(),
            ));
            continue;
        };
        let envelope = match Envelope::parse_diagnostic(event_name, &payload) {
            Ok(envelope) => envelope,
            Err(error) => {
                rejected += 1;
                rows.push(rejected_row(line_number, event_name, error.to_string()));
                continue;
            }
        };
        let warning = (!adapter.emits(event_name))
            .then(|| format!("event `{event_name}` is absent from manifest emits"));
        let _classification = adapter.classify_hook(event_name, &payload);

        if matches!(envelope.event, CanonicalEvent::Context) {
            let context_agent_id = match resolve_root_identity(
                adapter.descriptor.kind,
                event_name,
                envelope.agent_id.as_deref(),
                envelope.session_id.as_deref(),
            ) {
                RootIdentity::Root {
                    agent_id: Some(agent_id),
                } => agent_id,
                RootIdentity::Root { agent_id: None } | RootIdentity::ForeignChild => {
                    rejected += 1;
                    rows.push(rejected_row(
                        line_number,
                        event_name,
                        "context observation has no root agent identity".into(),
                    ));
                    continue;
                }
            };
            if adapter
                .observe_context(adapter.descriptor.kind, &payload)
                .is_none()
            {
                rejected += 1;
                rows.push(rejected_row(
                    line_number,
                    event_name,
                    "context observation rejected".into(),
                ));
                continue;
            }
            let state = states
                .get(context_agent_id.as_str())
                .map(state_label)
                .unwrap_or_else(|| "unchanged".into());
            rows.push(ReplayRow {
                line: line_number,
                event: event_name.to_owned(),
                signal: "context".into(),
                state,
                warning,
                error: None,
            });
            continue;
        }

        if matches!(envelope.event, CanonicalEvent::Unknown) {
            rows.push(ReplayRow {
                line: line_number,
                event: event_name.to_owned(),
                signal: "unknown".into(),
                state: "unchanged".into(),
                warning,
                error: None,
            });
            continue;
        }

        let Some(observation) = adapter.observe_lifecycle(event_name, &payload) else {
            rejected += 1;
            rows.push(rejected_row(
                line_number,
                event_name,
                "lifecycle observation rejected".into(),
            ));
            continue;
        };
        let signal = observation.signal.tag().to_owned();
        let Some(agent_id) = observation.agent_id.as_ref().map(ToString::to_string) else {
            rejected += 1;
            rows.push(rejected_row(
                line_number,
                event_name,
                "lifecycle observation has no agent identity".into(),
            ));
            continue;
        };
        let state = if matches!(observation.signal, LifecycleSignal::Ended) {
            states.remove(&agent_id);
            "ended".into()
        } else {
            let transition = step(states.get(&agent_id), None, &observation.signal);
            states.insert(agent_id, transition.next);
            state_label(&transition.next)
        };
        rows.push(ReplayRow {
            line: line_number,
            event: event_name.to_owned(),
            signal,
            state,
            warning,
            error: None,
        });
    }

    let final_states = states
        .into_iter()
        .map(|(agent_id, state)| ReplayFinalState {
            agent_id,
            status: state.status,
            phase: state.phase,
            compacting: state.compacting,
        })
        .collect();
    Ok(ReplayCheckReport {
        path: path.to_path_buf(),
        rows,
        final_states,
        rejected,
    })
}

fn rejected_row(line: usize, event: &str, error: String) -> ReplayRow {
    ReplayRow {
        line,
        event: event.to_owned(),
        signal: "rejected".into(),
        state: "unchanged".into(),
        warning: None,
        error: Some(error),
    }
}

fn state_label(state: &LifecycleState) -> String {
    let status = state.status.as_str();
    if state.compacting {
        return format!("{status}/compacting");
    }
    match state.phase {
        TurnPhase::Idle => status.into(),
        TurnPhase::Reasoning => format!("{status}/reasoning"),
        TurnPhase::Acting => format!("{status}/acting"),
        TurnPhase::Parked => format!("{status}/parked"),
    }
}
