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
use crate::disk::paths::StatePaths;
use crate::mux::LiveSessions;
use crate::workspace::record;

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
    claude: RuntimeControlReadiness,
    codex: RuntimeControlReadiness,
}

impl ReadinessSnapshot {
    pub fn probe(config: &RemoteControlConfig) -> Self {
        let login_env = crate::agents::ambient_env();
        Self::from_readiness(
            runtime_control::readiness("claude", config.enabled_for("claude"), &login_env),
            runtime_control::readiness("codex", config.enabled_for("codex"), &login_env),
        )
    }

    fn probe_transition(host: RemoteControlHost) -> Self {
        let login_env = crate::agents::ambient_env();
        match host {
            RemoteControlHost::Claude => Self::from_readiness(
                runtime_control::readiness("claude", true, &login_env),
                RuntimeControlReadiness::Disabled,
            ),
            RemoteControlHost::Codex => Self::from_readiness(
                RuntimeControlReadiness::Disabled,
                runtime_control::readiness("codex", true, &login_env),
            ),
        }
    }

    pub fn for_host(&self, host: RemoteControlHost) -> &RuntimeControlReadiness {
        match host {
            RemoteControlHost::Claude => &self.claude,
            RemoteControlHost::Codex => &self.codex,
        }
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
        Self { claude, codex }
    }
}

/// Seed the provider-side preconditions an enabled host needs before it
/// launches, so [`ReadinessSnapshot::probe`] judges each host on the state it
/// will actually start with. Best-effort and idempotent: a provider that cannot
/// fill its precondition reports it through readiness instead of failing here.
pub fn prepare_hosts(config: &RemoteControlConfig) {
    let login_env = crate::agents::ambient_env();
    for host in [RemoteControlHost::Claude, RemoteControlHost::Codex] {
        runtime_control::prepare(host.kind(), config.enabled_for(host.kind()), &login_env);
    }
}

/// Gate turning one host on before the config records it. Seeding runs first because the request to enable is the intent a host's own precondition needs; judging the pre-transition state would refuse the very configuration the toggle is about to create.
pub fn preflight_enable(host: RemoteControlHost) -> Result<(), RuntimeControlIssue> {
    let login_env = crate::agents::ambient_env();
    runtime_control::prepare(host.kind(), true, &login_env);
    ReadinessSnapshot::probe_transition(host).start_gate()
}

/// Advisory-only provider daemon findings. These never gate `rimz start`.
pub fn advisories(config: &RemoteControlConfig) -> Vec<String> {
    let login_env = crate::agents::ambient_env();
    let mut out = Vec::new();
    if config.enabled_for("codex")
        && let Some(skew) = runtime_control::updater_advisory("codex", &login_env)
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
    let login_env = crate::agents::ambient_env();
    if host == RemoteControlHost::Codex {
        runtime_control::reconcile(
            "codex",
            machine.remote_control.enabled_for("codex"),
            &login_env,
        )?;
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
            let record = match record::read(&paths.workspace_record) {
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
        let Ok(runtime) = crate::disk::paths::RuntimePaths::for_workspace(workspace.workspace_id)
        else {
            continue;
        };
        if let Err(err) = crate::wakeup::wake_store_delta(&runtime, None, None) {
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
pub(crate) fn claude_settings_path(login_env: &BTreeMap<String, String>) -> PathBuf {
    // The validated built-in Claude definition always supplies this input.
    runtime_control::wiring_input_path("claude", login_env)
        .expect("Claude runtime-control wiring input must be registered")
}

#[cfg(test)]
mod tests;
