//! Additive global Grok Build hook installation, refused while Grok's managed
//! policy keeps the hooks from running.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[cfg(test)]
use crate::agents::hook_types::HookEventSpec;
use crate::agents::managed_json_hooks::{ManagedJsonHookSpec, SyncEncoding};
use crate::agents::managed_source::{ManagedIntegration, ManagedSource};
use crate::agents::{
    AgentErr, AgentSpec, HookInstallPreview, HookInstallReport, HookPreflightErr,
    HookUninstallReport, Result,
};

use super::{GROK_HOOKS, RIMZ_HOOK_COMMAND, RIMZ_HOOK_MARKER};

static SPEC: ManagedJsonHookSpec = ManagedJsonHookSpec {
    agent: "grok",
    catalog: GROK_HOOKS,
    command: RIMZ_HOOK_COMMAND,
    legacy_command_marker: RIMZ_HOOK_MARKER,
    timeout: 4,
    sync: SyncEncoding::None,
    status_lines: &[],
};

pub(super) static MANAGED_SOURCE: ManagedSource =
    ManagedSource::json(&SPEC, super::paths::hooks_path);

#[cfg(test)]
pub(super) fn catalog() -> &'static [HookEventSpec] {
    GROK_HOOKS
}

pub(super) struct GrokManagedIntegration;

impl ManagedIntegration for GrokManagedIntegration {
    fn install(&self, env: &BTreeMap<String, String>) -> Result<HookInstallReport> {
        refuse_under_policy()?;
        MANAGED_SOURCE.install(env)
    }

    fn preview(&self, env: &BTreeMap<String, String>) -> Result<HookInstallPreview> {
        refuse_under_policy()?;
        MANAGED_SOURCE.preview(env)
    }

    fn uninstall(&self, env: &BTreeMap<String, String>) -> Result<HookUninstallReport> {
        MANAGED_SOURCE.uninstall(env)
    }

    fn installed(&self, env: &BTreeMap<String, String>) -> bool {
        MANAGED_SOURCE.installed(env)
    }

    fn managed_artifacts_present(&self, env: &BTreeMap<String, String>) -> bool {
        MANAGED_SOURCE.managed_artifacts_present(env)
    }

    fn upgrade_available(&self, env: &BTreeMap<String, String>) -> bool {
        MANAGED_SOURCE.upgrade_available(env)
    }

    fn wiring_input_paths(
        &self,
        descriptor: &AgentSpec,
        env: &BTreeMap<String, String>,
    ) -> Vec<PathBuf> {
        MANAGED_SOURCE.wiring_input_paths(descriptor, env)
    }

    fn install_blocker(&self) -> Option<String> {
        policy_blocker().map(|reason| format!("{reason}; {POLICY_FIX}"))
    }

    fn hooks_disabled(&self, _env: &BTreeMap<String, String>) -> Option<HookPreflightErr> {
        policy_blocker().map(|reason| HookPreflightErr::HooksDisabled {
            reason,
            fix: POLICY_FIX.to_owned(),
        })
    }
}

fn refuse_under_policy() -> Result<()> {
    match GrokManagedIntegration.install_blocker() {
        Some(reason) => Err(AgentErr::Install {
            agent: "grok",
            reason,
        }),
        None => Ok(()),
    }
}

const POLICY_FIX: &str = "an administrator adds `rimz hooks feed --source grok` as command hooks in /etc/grok/managed_config.toml or /etc/grok/requirements.toml";
const POLICY_KEYS: [&str; 2] = ["allowManagedHooksOnly", "allow_managed_hooks_only"];
const POLICY_FILES: [&str; 2] = ["managed_config.toml", "requirements.toml"];
const SYSTEM_DIR: &str = "/etc/grok";
#[cfg(target_os = "macos")]
const CLAUDE_MANAGED_SETTINGS: &str =
    "/Library/Application Support/ClaudeCode/managed-settings.json";
#[cfg(not(target_os = "macos"))]
const CLAUDE_MANAGED_SETTINGS: &str = "/etc/claude-code/managed-settings.json";

fn policy_blocker() -> Option<String> {
    policy_blocker_at(
        &machine_path(SYSTEM_DIR),
        &super::paths::home(),
        &machine_path(CLAUDE_MANAGED_SETTINGS),
    )
}

/// A machine-wide policy path. Test builds re-root it under
/// `RIMZ_TEST_GROK_POLICY_ROOT` so the suite never reads the host's own
/// policy; a shipped binary has no override for the managed-hooks pin.
fn machine_path(path: &str) -> PathBuf {
    #[cfg(feature = "testkit")]
    if let Some(root) =
        std::env::var_os("RIMZ_TEST_GROK_POLICY_ROOT").filter(|value| !value.is_empty())
    {
        return Path::new(&root).join(path.trim_start_matches('/'));
    }
    PathBuf::from(path)
}

/// Why RimZ's Grok hooks will not run: a policy layer pins Grok to managed
/// hooks only, and no system layer carries RimZ's handler. Mirrors upstream's
/// layer read: an unreadable or unparseable layer is skipped, while a
/// non-bool value or both spellings disagreeing engage the pin. A signed user
/// `requirements.toml` handler and macOS MDM policy are not read.
fn policy_blocker_at(system: &Path, user: &Path, claude: &Path) -> Option<String> {
    let toml_layers = [system, user]
        .into_iter()
        .flat_map(|dir| POLICY_FILES.map(|name| dir.join(name)))
        .filter_map(|path| Some((read_toml_layer(&path)?, path)));
    let claude_layer = std::fs::read_to_string(claude)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .map(|layer| (layer, claude.to_path_buf()));
    let (_, source) = toml_layers
        .chain(claude_layer)
        .find(|(layer, _)| pins_managed_hooks(layer))?;
    let handler_survives = POLICY_FILES
        .iter()
        .filter_map(|name| read_toml_layer(&system.join(name)))
        .any(|layer| layer.get("hooks").is_some_and(carries_rimz_handler));
    (!handler_survives).then(|| {
        format!(
            "Grok's managed policy (`allow_managed_hooks_only`) is engaged by {}, so RimZ's Grok hooks will not run",
            source.display()
        )
    })
}

fn read_toml_layer(path: &Path) -> Option<serde_json::Value> {
    let table: toml::Table = toml::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    serde_json::to_value(table).ok()
}

fn pins_managed_hooks(layer: &serde_json::Value) -> bool {
    let mut values = POLICY_KEYS.iter().filter_map(|key| layer.get(key));
    let Some(first) = values.next() else {
        return false;
    };
    values.any(|value| value != first) || first.as_bool() != Some(false)
}

fn carries_rimz_handler(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Array(items) => items.iter().any(carries_rimz_handler),
        serde_json::Value::Object(fields) => fields.iter().any(|(key, value)| {
            (key == "command" && value.as_str().is_some_and(|c| c.contains(RIMZ_HOOK_MARKER)))
                || carries_rimz_handler(value)
        }),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_policy_layers_and_surviving_handlers() {
        let dir = tempfile::tempdir().unwrap();
        let system = dir.path().join("system");
        let user = dir.path().join("user");
        let claude = dir.path().join("managed-settings.json");
        std::fs::create_dir(&system).unwrap();
        std::fs::create_dir(&user).unwrap();
        let layers = [
            system.join("managed_config.toml"),
            system.join("requirements.toml"),
            user.join("managed_config.toml"),
            user.join("requirements.toml"),
            claude.clone(),
        ];
        for path in &layers {
            for key in ["allow_managed_hooks_only", "allowManagedHooksOnly"] {
                for value in [serde_json::json!(true), serde_json::json!("not-a-bool")] {
                    let content = if path == &claude {
                        serde_json::json!({(key):value}).to_string()
                    } else {
                        format!("{key} = {value}")
                    };
                    std::fs::write(path, content).unwrap();
                    let reason =
                        policy_blocker_at(&system, &user, &claude).expect("policy engaged");
                    assert!(reason.contains(&path.display().to_string()));
                    std::fs::remove_file(path).unwrap();
                }
            }
        }
        let pin = user.join("managed_config.toml");
        std::fs::write(&pin, "allow_managed_hooks_only = true").unwrap();
        let handler = "\n[hooks]\nSessionStart = [{ hooks = [{ type = 'command', command = 'rimz hooks feed --source grok' }] }]\n";
        std::fs::write(&pin, format!("allow_managed_hooks_only = true{handler}")).unwrap();
        assert!(
            policy_blocker_at(&system, &user, &claude).is_some(),
            "user handler does not survive"
        );
        for name in ["managed_config.toml", "requirements.toml"] {
            let path = system.join(name);
            std::fs::write(&path, handler).unwrap();
            assert!(
                policy_blocker_at(&system, &user, &claude).is_none(),
                "system handler survives"
            );
            std::fs::remove_file(path).unwrap();
        }
        std::fs::remove_file(&pin).unwrap();
        for path in &layers {
            std::fs::write(path, "{").unwrap();
            assert!(
                policy_blocker_at(&system, &user, &claude).is_none(),
                "malformed layer skipped"
            );
            std::fs::write(
                path,
                if path == &claude {
                    r#"{"allowManagedHooksOnly":false}"#
                } else {
                    "allow_managed_hooks_only = false"
                },
            )
            .unwrap();
            assert!(policy_blocker_at(&system, &user, &claude).is_none());
            std::fs::remove_file(path).unwrap();
        }
        assert!(policy_blocker_at(&system, &user, &claude).is_none());
        std::fs::write(
            &claude,
            r#"{"allowManagedHooksOnly":false,"allow_managed_hooks_only":true}"#,
        )
        .unwrap();
        assert!(
            policy_blocker_at(&system, &user, &claude).is_some(),
            "conflicting spellings engage"
        );
    }
}
