//! Managed JSON hooks with the closed-event-schema release floor.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::{QODER_HOOKS, QoderAdapter};
use crate::agents::capabilities::LaunchCapability;
use crate::agents::managed_json_hooks::{ManagedJsonHookSpec, SyncEncoding};
use crate::agents::managed_source::{ManagedIntegration, ManagedSource};
use crate::agents::version::{CliVersion, probe_cli_version};
use crate::agents::{
    AgentErr, AgentSpec, HookInstallPreview, HookInstallReport, HookUninstallReport, Result,
};

pub(super) const MIN_VERSION: CliVersion = CliVersion::new(1, 1, 72);
static SPEC: ManagedJsonHookSpec = ManagedJsonHookSpec {
    agent: "qoder",
    catalog: QODER_HOOKS,
    command: "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source qoder",
    legacy_command_marker: "rimz hooks feed --source qoder",
    timeout: 2,
    sync: SyncEncoding::HandlerAsync,
    status_lines: &[],
};
static SOURCE: ManagedSource = ManagedSource::json(&SPEC, settings_path);

pub(super) fn settings_path(env: &BTreeMap<String, String>) -> Result<PathBuf> {
    QoderAdapter
        .config_home(env)
        .map(|home| home.join("settings.json"))
        .ok_or_else(|| AgentErr::Install {
            agent: "qoder",
            reason: "set QODER_CONFIG_DIR or HOME to resolve Qoder settings".into(),
        })
}

impl ManagedIntegration for QoderAdapter {
    fn install(&self, env: &BTreeMap<String, String>) -> Result<HookInstallReport> {
        if let Some(reason) = self.install_blocker(env) {
            return Err(AgentErr::Install {
                agent: "qoder",
                reason,
            });
        }
        SOURCE.install(env)
    }
    fn preview(&self, env: &BTreeMap<String, String>) -> Result<HookInstallPreview> {
        if let Some(reason) = self.install_blocker(env) {
            return Err(AgentErr::Install {
                agent: "qoder",
                reason,
            });
        }
        SOURCE.preview(env)
    }
    fn uninstall(&self, env: &BTreeMap<String, String>) -> Result<HookUninstallReport> {
        SOURCE.uninstall(env)
    }
    fn installed(&self, env: &BTreeMap<String, String>) -> bool {
        SOURCE.installed(env)
    }
    fn managed_artifacts_present(&self, env: &BTreeMap<String, String>) -> bool {
        SOURCE.managed_artifacts_present(env)
    }
    fn wiring_input_paths(&self, spec: &AgentSpec, env: &BTreeMap<String, String>) -> Vec<PathBuf> {
        ManagedIntegration::wiring_input_paths(&SOURCE, spec, env)
    }
    fn install_blocker(&self, _env: &BTreeMap<String, String>) -> Option<String> {
        let found: CliVersion = probe_cli_version("qodercli")?.parse().ok()?;
        (found < MIN_VERSION).then(|| format!("RimZ's Qoder integration needs qodercli {MIN_VERSION} or later (found {found}); run `qodercli update`"))
    }
}
