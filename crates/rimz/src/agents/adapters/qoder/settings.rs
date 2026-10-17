//! Read-only layered hook preconditions for Qoder launches.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::agents::capabilities::{LaunchBlocker, LaunchCapability};

pub(super) fn hook_blocker(
    cwd: &Path,
    env: &BTreeMap<String, String>,
    argv: &[String],
) -> Option<LaunchBlocker> {
    for key in ["QODER_CLI_HOME", "QODER_CONFIG_DIR_NAME"] {
        if env.contains_key(key) {
            return Some(LaunchBlocker(format!(
                "Qoder's {key} selects a different hook home; unset {key} and use QODER_CONFIG_DIR instead"
            )));
        }
    }
    let mut args = argv.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--" => break,
            "-i" | "--prompt-interactive" => {
                args.next();
            }
            "--config-dir" => return Some(config_dir_refusal()),
            _ if arg.starts_with("--config-dir=") => return Some(config_dir_refusal()),
            _ => {}
        }
    }
    let mut paths = super::QoderAdapter
        .config_home(env)
        .map(|home| home.join("settings.json"))
        .into_iter()
        .collect::<Vec<_>>();
    paths.extend([
        cwd.join(".qoder/settings.json"),
        cwd.join(".qoder/settings.local.json"),
    ]);
    let layers = paths
        .into_iter()
        .filter_map(|path| {
            let bytes = std::fs::read(&path).ok()?;
            let value: Value = crate::agents::jsonc::from_slice(&bytes).ok()?;
            Some((path, value))
        })
        .collect::<Vec<_>>();
    for (pointer, setting, disabled, fix) in [
        ("/disableAllHooks", "disableAllHooks", true, "false"),
        ("/hooksConfig/enabled", "hooksConfig.enabled", false, "true"),
    ] {
        if let Some((path, value)) = layers
            .iter()
            .rev()
            .find(|(_, value)| value.pointer(pointer).is_some())
            && value.pointer(pointer).and_then(Value::as_bool) == Some(disabled)
        {
            return Some(LaunchBlocker(format!(
                "Qoder hooks are disabled by {setting} in {}; set {setting} to {fix} there so RimZ can track turns and prompts",
                path.display()
            )));
        }
    }
    None
}

fn config_dir_refusal() -> LaunchBlocker {
    LaunchBlocker("Qoder's --config-dir selects a different hook home; remove --config-dir and use QODER_CONFIG_DIR instead".into())
}
