//! Read-only Copilot settings precedence for launch and hook health.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::agents::SettingsConflict;
use crate::agents::capabilities::LaunchCapability;

pub(super) fn read_jsonc_object(path: &Path) -> Option<Map<String, Value>> {
    let value = crate::agents::jsonc::from_slice::<Value>(&std::fs::read(path).ok()?).ok()?;
    value.as_object().cloned()
}

fn user_settings(env: &BTreeMap<String, String>) -> Option<PathBuf> {
    env.get("RIMZ_COPILOT_SETTINGS")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            super::CopilotAdapter
                .config_home(env)
                .map(|home| home.join("settings.json"))
        })
}

fn effective_setting(
    cwd: &Path,
    env: &BTreeMap<String, String>,
    key: &str,
) -> Option<(PathBuf, Value)> {
    let root = cwd
        .ancestors()
        .find(|path| path.join(".git").exists())
        .unwrap_or(cwd);
    [
        Some(root.join(".github/copilot/settings.local.json")),
        Some(root.join(".github/copilot/settings.json")),
        user_settings(env),
    ]
    .into_iter()
    .flatten()
    .find_map(|path| {
        let value = read_jsonc_object(&path)?.remove(key)?;
        Some((path, value))
    })
}

pub(super) fn hooks_disabled(
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> Option<SettingsConflict> {
    let (path, value) = effective_setting(cwd, env, "disableAllHooks")?;
    if value.as_bool() != Some(true) {
        return None;
    }
    Some(SettingsConflict {
        fix: format!(
            "set disableAllHooks to false or remove it in {}",
            path.display()
        ),
        path,
        key: "disableAllHooks",
        value,
    })
}

pub(super) fn ask_unenforced(
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> Option<SettingsConflict> {
    let (path, value) = effective_setting(cwd, env, "defaultPermissionMode")?;
    if value.as_str()? == "manual" {
        return None;
    }
    let fix = if user_settings(env).as_ref() == Some(&path) {
        let default = env.get("HOME").map(|home| Path::new(home).join(".copilot"));
        let prefix = super::CopilotAdapter
            .config_home(env)
            .filter(|home| default.as_ref() != Some(home))
            .and_then(|home| {
                shlex::try_quote(&home.to_string_lossy())
                    .ok()
                    .map(|home| format!("COPILOT_HOME={home} "))
            })
            .unwrap_or_default();
        format!("run `{prefix}copilot config defaultPermissionMode manual`")
    } else {
        format!("set defaultPermissionMode to manual in {}", path.display())
    };
    Some(SettingsConflict {
        path,
        key: "defaultPermissionMode",
        value,
        fix,
    })
}
