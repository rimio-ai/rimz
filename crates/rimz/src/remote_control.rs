//! Provider-neutral remote-control readiness, room lifecycle, and sidebar wakes.
//!
//! Claude owns its foreground host protocol. Codex owns its managed per-user
//! daemon protocol. This module probes each provider once per operation, maps
//! their native readiness through explicit matches, and coordinates effects.

use std::path::PathBuf;

use crate::agents::claude::remote_control as claude;
use crate::agents::codex::app_server::daemon as codex;
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
    claude: HostState,
    codex: HostState,
    claude_host_argv: Option<Vec<String>>,
}

impl ReadinessSnapshot {
    pub fn probe(config: &RemoteControlConfig) -> Self {
        let (claude, claude_host_argv) = probe_claude(config.claude);
        let codex = probe_codex(config.codex);
        Self {
            claude,
            codex,
            claude_host_argv,
        }
    }

    pub fn probe_transition(host: RemoteControlHost) -> Self {
        match host {
            RemoteControlHost::Claude => {
                let (claude, claude_host_argv) = probe_claude(true);
                Self {
                    claude,
                    codex: HostState::Disabled,
                    claude_host_argv,
                }
            }
            RemoteControlHost::Codex => Self {
                claude: HostState::Disabled,
                codex: probe_codex(true),
                claude_host_argv: None,
            },
        }
    }

    pub fn for_host(&self, host: RemoteControlHost) -> &HostState {
        match host {
            RemoteControlHost::Claude => &self.claude,
            RemoteControlHost::Codex => &self.codex,
        }
    }

    pub fn claude_host_argv(&self) -> Option<&[String]> {
        self.claude_host_argv.as_deref()
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
        let claude_host_argv = matches!(claude, HostState::Ready).then(claude::host_argv);
        Self {
            claude,
            codex,
            claude_host_argv,
        }
    }
}

fn probe_claude(enabled: bool) -> (HostState, Option<Vec<String>>) {
    match claude::readiness(enabled) {
        claude::Readiness::Disabled => (HostState::Disabled, None),
        claude::Readiness::Ready { host_argv } => (HostState::Ready, Some(host_argv)),
        claude::Readiness::Uninstalled(issue) => {
            (HostState::Uninstalled(PreflightError::Claude(issue)), None)
        }
        claude::Readiness::Blocked(issue) => {
            (HostState::Blocked(PreflightError::Claude(issue)), None)
        }
    }
}

fn probe_codex(enabled: bool) -> HostState {
    match codex::readiness(enabled) {
        codex::Readiness::Disabled => HostState::Disabled,
        codex::Readiness::Ready => HostState::Ready,
        codex::Readiness::Uninstalled(issue) => {
            HostState::Uninstalled(PreflightError::Codex(issue))
        }
    }
}

/// Advisory-only provider daemon findings. These never gate `rimz start`.
pub fn advisories(config: &RemoteControlConfig) -> Vec<String> {
    let mut out = Vec::new();
    if config.codex
        && let Some(skew) = codex::updater_skew()
    {
        out.push(skew.to_string());
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreflightError {
    Claude(claude::Issue),
    Codex(codex::Issue),
}

impl PreflightError {
    pub fn is_uninstalled_host(&self) -> bool {
        matches!(
            self,
            Self::Claude(claude::Issue::Uninstalled) | Self::Codex(codex::Issue::StandaloneMissing)
        )
    }
}

impl std::fmt::Display for PreflightError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Claude(issue) => issue.fmt(f),
            Self::Codex(issue) => issue.fmt(f),
        }
    }
}

impl std::error::Error for PreflightError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Claude(issue) => Some(issue),
            Self::Codex(issue) => Some(issue),
        }
    }
}

/// Apply one persisted runtime toggle across provider lifecycle, live Claude
/// room panes, and every known workspace's sidebar.
pub fn apply_runtime_toggle(
    host: RemoteControlHost,
    machine: &crate::config::MachineConfig,
) -> Result<(), codex::ControlError> {
    if host == RemoteControlHost::Codex {
        codex::reconcile(machine.remote_control.codex)?;
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
    claude::settings_path()
}

#[cfg(test)]
mod tests;
