//! Provider-neutral remote-control readiness, room lifecycle, and sidebar wakes.
//!
//! Claude owns its foreground host protocol. Codex owns its managed per-user
//! daemon protocol. This module probes each provider once per operation and
//! coordinates effects.

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

impl RemoteControlHost {
    pub const fn kind(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

/// One batch probe of both configured provider hosts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadinessSnapshot {
    states: BTreeMap<crate::ids::AgentKind, RuntimeControlReadiness>,
}

impl ReadinessSnapshot {
    pub fn probe(config: &RemoteControlConfig) -> Self {
        Self::from_readiness(
            runtime_control::readiness("claude", config.enabled_for("claude")),
            runtime_control::readiness("codex", config.enabled_for("codex")),
        )
    }

    pub fn probe_transition(host: RemoteControlHost) -> Self {
        match host {
            RemoteControlHost::Claude => Self::from_readiness(
                runtime_control::readiness("claude", true),
                RuntimeControlReadiness::Disabled,
            ),
            RemoteControlHost::Codex => Self::from_readiness(
                RuntimeControlReadiness::Disabled,
                runtime_control::readiness("codex", true),
            ),
        }
    }

    pub fn for_host(&self, host: RemoteControlHost) -> &RuntimeControlReadiness {
        self.for_kind(host.kind())
    }

    pub fn for_kind(&self, kind: &str) -> &RuntimeControlReadiness {
        self.states
            .get(&crate::ids::AgentKind::new_unchecked(kind))
            .unwrap_or(&RuntimeControlReadiness::Disabled)
    }

    pub fn claude_host_argv(&self) -> Option<&[String]> {
        match self.for_host(RemoteControlHost::Claude) {
            RuntimeControlReadiness::Ready {
                host_argv: Some(argv),
            } => Some(argv),
            _ => None,
        }
    }

    /// Skip uninstalled providers and refuse the first installed-provider block.
    pub fn start_gate(&self) -> Result<(), RuntimeControlIssue> {
        for host in [RemoteControlHost::Codex, RemoteControlHost::Claude] {
            if let RuntimeControlReadiness::Blocked(issue) = self.for_host(host) {
                return Err(issue.clone());
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn from_states(
        claude: RuntimeControlReadiness,
        codex: RuntimeControlReadiness,
    ) -> Self {
        Self::from_readiness(claude, codex)
    }

    fn from_readiness(claude: RuntimeControlReadiness, codex: RuntimeControlReadiness) -> Self {
        Self {
            states: [("claude", claude), ("codex", codex)]
                .into_iter()
                .map(|(kind, state)| (crate::ids::AgentKind::new_unchecked(kind), state))
                .collect(),
        }
    }
}

/// Seed the provider-side preconditions an enabled host needs before it
/// launches, so [`ReadinessSnapshot::probe`] judges each host on the state it
/// will actually start with. Best-effort and idempotent: a provider that cannot
/// fill its precondition reports it through readiness instead of failing here.
pub fn prepare_hosts(config: &RemoteControlConfig) {
    for host in [RemoteControlHost::Claude, RemoteControlHost::Codex] {
        runtime_control::prepare(host.kind(), config.enabled_for(host.kind()));
    }
}

/// Prepare one host as if already enabled, for a transition the config has not
/// recorded yet. Turning a toggle on is the intent that authorizes the seed, so
/// this runs before the gate judges whether that host can serve.
pub fn prepare_host(host: RemoteControlHost) {
    runtime_control::prepare(host.kind(), true);
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
        prepare_hosts(&machine.remote_control);
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
