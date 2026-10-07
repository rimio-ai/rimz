//! Provider-neutral remote-control readiness, room lifecycle, and sidebar wakes.
//!
//! Claude owns its foreground host protocol. Codex owns its managed per-user
//! daemon protocol. This module probes each provider once per operation and
//! coordinates effects.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::agents::runtime_control::{
    self, RuntimeControlError, RuntimeControlIssue, RuntimeControlReadiness,
};
use crate::config::{AccountsConfig, RemoteControlConfig};
use crate::disk::paths::StatePaths;
use crate::ids::{AgentKind, LoginKey};
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

/// The provider environment each remote-control host of one room runs under.
#[derive(Clone, Debug)]
pub struct HostLoginEnvs {
    claude: Result<BTreeMap<String, String>, Arc<crate::agents::RoomLoginErr>>,
    codex: Result<BTreeMap<String, String>, Arc<crate::agents::RoomLoginErr>>,
}

impl Default for HostLoginEnvs {
    fn default() -> Self {
        Self {
            claude: Ok(BTreeMap::new()),
            codex: Ok(BTreeMap::new()),
        }
    }
}

impl HostLoginEnvs {
    pub fn ambient() -> Self {
        let ambient = crate::agents::ambient_env();
        Self {
            claude: Ok(ambient.clone()),
            codex: Ok(ambient),
        }
    }

    pub fn from_logins(accounts: &AccountsConfig, logins: &crate::agents::RoomAccounts) -> Self {
        let ambient = crate::agents::ambient_env();
        Self {
            claude: logins
                .login(&AgentKind::new_unchecked("claude"), accounts)
                .map(|login| login.env(&ambient))
                .map_err(Arc::new),
            codex: logins
                .login(&AgentKind::new_unchecked("codex"), accounts)
                .map(|login| login.env(&ambient))
                .map_err(Arc::new),
        }
    }

    pub fn for_room(
        record: &Path,
        machine: &crate::config::MachineConfig,
    ) -> Result<Self, crate::agents::RoomLoginErr> {
        let logins = crate::agents::room_accounts(record, None, machine)?;
        Ok(Self::from_logins(&machine.accounts, &logins))
    }

    pub fn for_host(
        &self,
        host: RemoteControlHost,
    ) -> Result<&BTreeMap<String, String>, &Arc<crate::agents::RoomLoginErr>> {
        match host {
            RemoteControlHost::Claude => self.claude.as_ref(),
            RemoteControlHost::Codex => self.codex.as_ref(),
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
    pub(crate) fn probe_with(
        config: &RemoteControlConfig,
        envs: &HostLoginEnvs,
        mut probe: impl FnMut(&str, bool, &BTreeMap<String, String>) -> RuntimeControlReadiness,
    ) -> Self {
        let mut readiness = |host: RemoteControlHost| match envs.for_host(host) {
            Ok(env) => probe(host.kind(), config.enabled_for(host.kind()), env),
            Err(error) if config.enabled_for(host.kind()) => RuntimeControlReadiness::Blocked(
                RuntimeControlIssue::new(host.kind(), "account_unavailable", error),
            ),
            Err(_) => RuntimeControlReadiness::Disabled,
        };
        Self::from_readiness(
            readiness(RemoteControlHost::Claude),
            readiness(RemoteControlHost::Codex),
        )
    }

    pub fn probe(config: &RemoteControlConfig, envs: &HostLoginEnvs) -> Self {
        Self::probe_with(config, envs, runtime_control::readiness)
    }

    pub(crate) fn disabled() -> Self {
        Self::from_readiness(
            RuntimeControlReadiness::Disabled,
            RuntimeControlReadiness::Disabled,
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
pub fn prepare_hosts(config: &RemoteControlConfig, envs: &HostLoginEnvs) {
    for host in [RemoteControlHost::Claude, RemoteControlHost::Codex] {
        let Ok(env) = envs.for_host(host) else {
            continue;
        };
        runtime_control::prepare(host.kind(), config.enabled_for(host.kind()), env);
    }
}

/// Gate turning one host on before the config records it. Seeding runs first because the request to enable is the intent a host's own precondition needs; judging the pre-transition state would refuse the very configuration the toggle is about to create.
pub fn preflight_enable(host: RemoteControlHost) -> Result<(), RuntimeControlIssue> {
    let login_env = crate::agents::ambient_env();
    runtime_control::prepare(host.kind(), true, &login_env);
    ReadinessSnapshot::probe_transition(host).start_gate()
}

/// Advisory-only provider daemon findings. These never gate `rimz start`.
pub fn advisories(config: &RemoteControlConfig, envs: &HostLoginEnvs) -> Vec<String> {
    let mut out = Vec::new();
    if config.enabled_for("codex")
        && let Ok(env) = envs.for_host(RemoteControlHost::Codex)
        && let Some(skew) = runtime_control::updater_advisory("codex", env)
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
    let workspaces = crate::workspace::known_workspaces();
    if host == RemoteControlHost::Codex {
        let enabled = machine.remote_control.enabled_for("codex");
        for login_env in
            codex_daemon_envs(machine, workspaces.as_deref().unwrap_or_default(), enabled)
        {
            runtime_control::reconcile("codex", enabled, &login_env)?;
        }
    }

    let workspaces = match workspaces {
        Ok(workspaces) => workspaces,
        Err(err) => {
            tracing::warn!(error = %err, "remote-control toggle could not enumerate workspaces");
            return Ok(());
        }
    };

    if host == RemoteControlHost::Claude {
        let live = LiveSessions::probe();
        for workspace in &workspaces {
            let Some(mux) = live.mux_of(&workspace.session_name) else {
                continue;
            };
            let paths = match StatePaths::for_project_root(&workspace.project_root) {
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
            let envs = match HostLoginEnvs::for_room(&paths.workspace_record, machine) {
                Ok(envs) => envs,
                Err(err) => {
                    tracing::debug!(
                        workspace = %workspace.workspace_id,
                        error = &err as &dyn std::error::Error,
                        "remote-control toggle skipped a workspace with unavailable accounts",
                    );
                    continue;
                }
            };
            prepare_hosts(&machine.remote_control, &envs);
            let readiness = ReadinessSnapshot::probe(&machine.remote_control, &envs);
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
        let Ok(runtime) =
            crate::disk::paths::RuntimePaths::for_project_root(&workspace.project_root)
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

/// The Codex homes a toggle reconciles, one per account: the provider's own
/// home and every room's account, plus, when turning off, every declared
/// account, so a daemon a reset room left behind stops too.
fn codex_daemon_envs(
    machine: &crate::config::MachineConfig,
    workspaces: &[crate::workspace::KnownWorkspace],
    enabled: bool,
) -> Vec<BTreeMap<String, String>> {
    let codex = AgentKind::new_unchecked("codex");
    let ambient = crate::agents::ambient_env();
    let mut envs = BTreeMap::from([(LoginKey::default_for(codex.clone()), ambient.clone())]);
    if let Ok(catalog) = crate::agents::LoginCatalog::from_config(&machine.accounts)
        && !enabled
    {
        for login in catalog.all().filter(|login| login.kind() == &codex) {
            envs.insert(login.key(), login.env(&ambient));
        }
    }
    for workspace in workspaces {
        let Ok(paths) = StatePaths::for_project_root(&workspace.project_root) else {
            continue;
        };
        if let Ok(login) = crate::agents::room_account(&paths.workspace_record, machine, &codex) {
            envs.insert(login.key(), login.env(&ambient));
        }
    }
    envs.into_values().collect()
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
