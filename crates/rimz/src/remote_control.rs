//! Provider-neutral remote-control readiness, room lifecycle, and sidebar wakes.
//!
//! Claude owns its foreground host protocol. Codex owns its managed per-user
//! daemon protocol. This module probes each provider once per operation, maps
//! their native readiness through explicit matches, and coordinates effects.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::agents::runtime_control::{
    self, RuntimeControlError, RuntimeControlIssue, RuntimeControlReadiness,
};
use crate::config::RemoteControlConfig;
use crate::room::session::LiveSessions;
use crate::store::{paths::StatePaths, workspace_record};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteControlHost {
    Claude,
    Codex,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostState {
    Disabled,
    Ready,
    Uninstalled(PreflightError),
    Blocked(PreflightError),
}

/// One batch probe of both configured provider hosts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadinessSnapshot {
    states: BTreeMap<crate::ids::AgentKind, HostState>,
    host_argv: BTreeMap<crate::ids::AgentKind, Vec<String>>,
}

impl ReadinessSnapshot {
    pub fn probe(config: &RemoteControlConfig) -> Self {
        let (claude, claude_host_argv) = probe_claude(config.enabled_for("claude"));
        let codex = probe_codex(config.enabled_for("codex"));
        Self::from_probes(
            [("claude", claude), ("codex", codex)],
            claude_host_argv.map(|argv| ("claude", argv)),
        )
    }

    pub fn probe_transition(host: RemoteControlHost) -> Self {
        match host {
            RemoteControlHost::Claude => {
                let (claude, claude_host_argv) = probe_claude(true);
                Self::from_probes(
                    [("claude", claude), ("codex", HostState::Disabled)],
                    claude_host_argv.map(|argv| ("claude", argv)),
                )
            }
            RemoteControlHost::Codex => Self::from_probes(
                [
                    ("claude", HostState::Disabled),
                    ("codex", probe_codex(true)),
                ],
                None,
            ),
        }
    }

    pub fn for_host(&self, host: RemoteControlHost) -> &HostState {
        self.for_kind(match host {
            RemoteControlHost::Claude => "claude",
            RemoteControlHost::Codex => "codex",
        })
    }

    pub fn for_kind(&self, kind: &str) -> &HostState {
        self.states
            .get(&crate::ids::AgentKind::new_unchecked(kind))
            .unwrap_or(&HostState::Disabled)
    }

    pub fn claude_host_argv(&self) -> Option<&[String]> {
        self.host_argv
            .get(&crate::ids::AgentKind::new_unchecked("claude"))
            .map(Vec::as_slice)
    }

    /// Skip uninstalled providers and refuse the first installed-provider block.
    pub fn start_gate(&self) -> Result<(), PreflightError> {
        for host in [RemoteControlHost::Codex, RemoteControlHost::Claude] {
            if let HostState::Blocked(issue) = self.for_host(host) {
                return Err(issue.clone());
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn from_states(claude: HostState, codex: HostState) -> Self {
        let claude_host_argv = matches!(claude, HostState::Ready)
            .then(|| runtime_control::host_argv("claude"))
            .flatten();
        Self::from_probes(
            [("claude", claude), ("codex", codex)],
            claude_host_argv.map(|argv| ("claude", argv)),
        )
    }

    fn from_probes(
        states: impl IntoIterator<Item = (&'static str, HostState)>,
        host_argv: Option<(&'static str, Vec<String>)>,
    ) -> Self {
        Self {
            states: states
                .into_iter()
                .map(|(kind, state)| (crate::ids::AgentKind::new_unchecked(kind), state))
                .collect(),
            host_argv: host_argv
                .into_iter()
                .map(|(kind, argv)| (crate::ids::AgentKind::new_unchecked(kind), argv))
                .collect(),
        }
    }
}

fn probe_claude(enabled: bool) -> (HostState, Option<Vec<String>>) {
    match runtime_control::readiness("claude", enabled) {
        RuntimeControlReadiness::Disabled => (HostState::Disabled, None),
        RuntimeControlReadiness::Ready { host_argv } => (HostState::Ready, host_argv),
        RuntimeControlReadiness::Uninstalled(issue) => (HostState::Uninstalled(issue), None),
        RuntimeControlReadiness::Blocked(issue) => (HostState::Blocked(issue), None),
    }
}

fn probe_codex(enabled: bool) -> HostState {
    match runtime_control::readiness("codex", enabled) {
        RuntimeControlReadiness::Disabled => HostState::Disabled,
        RuntimeControlReadiness::Ready { .. } => HostState::Ready,
        RuntimeControlReadiness::Uninstalled(issue) => HostState::Uninstalled(issue),
        RuntimeControlReadiness::Blocked(issue) => HostState::Blocked(issue),
    }
}

/// Advisory-only provider daemon findings. These never gate `rimz start`.
pub fn advisories(config: &RemoteControlConfig) -> Vec<String> {
    let mut out = Vec::new();
    if config.enabled_for("codex")
        && let Some(skew) = runtime_control::updater_advisory("codex")
    {
        out.push(skew);
    }
    out
}

pub type PreflightError = RuntimeControlIssue;

/// Apply one persisted runtime toggle across provider lifecycle, live Claude
/// room panes, and every known workspace's sidebar.
pub fn apply_runtime_toggle(
    host: RemoteControlHost,
    machine: &crate::config::MachineConfig,
) -> Result<(), RuntimeControlError> {
    if host == RemoteControlHost::Codex {
        runtime_control::reconcile("codex", machine.remote_control.enabled_for("codex"))?;
    }

    let workspaces = match crate::workspace::known_workspaces() {
        Ok(workspaces) => workspaces,
        Err(err) => {
            tracing::warn!(error = %err, "remote-control toggle could not enumerate workspaces");
            return Ok(());
        }
    };

    if host == RemoteControlHost::Claude {
        let live = LiveSessions::probe();
        let readiness = ReadinessSnapshot::probe(&machine.remote_control);
        for workspace in &workspaces {
            let Some(mux) = live.mux_of(&workspace.session_name) else {
                continue;
            };
            let paths = match StatePaths::for_workspace(workspace.workspace_id.clone()) {
                Ok(paths) => paths,
                Err(err) => {
                    tracing::debug!(
                        workspace = %workspace.workspace_id,
                        error = &err as &dyn std::error::Error,
                        "remote-control toggle skipped a workspace with unavailable state paths",
                    );
                    continue;
                }
            };
            let record = match workspace_record::read(&paths.workspace_record) {
                Ok(record) => record,
                Err(err) => {
                    tracing::debug!(
                        workspace = %workspace.workspace_id,
                        error = &err as &dyn std::error::Error,
                        "remote-control toggle skipped a workspace with unavailable metadata",
                    );
                    continue;
                }
            };
            let backend = crate::mux::backend_for(mux);
            crate::daemon_view::ensure_daemon_view_with_readiness(
                backend.as_ref(),
                &workspace.workspace_id,
                &workspace.session_name,
                &record,
                machine,
                &readiness,
            );
        }
    }

    for workspace in workspaces {
        let Ok(runtime) = crate::store::RuntimePaths::for_workspace(workspace.workspace_id) else {
            continue;
        };
        if let Err(err) = crate::store::wakeup::wake_sidebars(&runtime) {
            tracing::debug!(
                workspace = %runtime.workspace_id,
                error = &err as &dyn std::error::Error,
                "remote-control toggle could not wake sidebars",
            );
        }
    }
    Ok(())
}

/// Claude settings input used by readiness and daemon repair invalidation.
/// Resolving the path performs no parsing or CLI probe.
pub(crate) fn claude_settings_path() -> PathBuf {
    // The validated built-in Claude definition always supplies this input.
    runtime_control::wiring_input_path("claude")
        .expect("Claude runtime-control wiring input must be registered")
}

#[cfg(test)]
mod tests;
