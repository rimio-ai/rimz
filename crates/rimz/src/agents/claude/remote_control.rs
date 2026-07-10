//! Claude Code remote-control settings and version gates.
//!
//! This module owns upstream facts so `rimz start` preflight, doctor, and the
//! sidebar badge all read one source.

use serde_json::{Map, Value};
use std::path::PathBuf;

use crate::agents::version::{CliVersion, probe_cli_version};

use super::install::{claude_settings_path, read_existing_json};

pub(crate) const MIN_REMOTE_CONTROL: CliVersion = CliVersion::new(2, 1, 51);
pub(crate) const AUTH_ENV_BLOCKS_RC_SINCE: CliVersion = CliVersion::new(2, 1, 157);
pub(crate) const CUSTOM_ENDPOINT_BLOCKS_RC_SINCE: CliVersion = CliVersion::new(2, 1, 196);

const FALLBACK_SETTINGS_PATH: &str = "~/.claude/settings.json";
const ANTHROPIC_API_KEY: &str = "ANTHROPIC_API_KEY";
const ANTHROPIC_AUTH_TOKEN: &str = "ANTHROPIC_AUTH_TOKEN";
const ANTHROPIC_BASE_URL: &str = "ANTHROPIC_BASE_URL";
const THIRD_PARTY_PROVIDER_VARS: [&str; 3] = [
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ClaudeRcSettings {
    pub disable_remote_control: bool,
    pub remote_control_at_startup: bool,
    pub api_key_helper: bool,
    pub env_auth_conflict: bool,
    pub env_endpoint_conflict: bool,
}

pub(crate) fn read_rc_settings() -> (PathBuf, ClaudeRcSettings) {
    let path = match claude_settings_path() {
        Ok(path) => path,
        Err(err) => {
            tracing::warn!(error = %err, "Claude settings path unavailable for remote-control read");
            return (
                PathBuf::from(FALLBACK_SETTINGS_PATH),
                ClaudeRcSettings::default(),
            );
        }
    };
    let settings = match read_existing_json(&path) {
        Ok(root) => rc_settings_from(&root),
        Err(err) => {
            tracing::warn!(
                path = %path.display(),
                error = %err,
                "Claude settings unreadable for remote-control read",
            );
            ClaudeRcSettings::default()
        }
    };
    (path, settings)
}

pub(crate) fn rc_settings_from(root: &Map<String, Value>) -> ClaudeRcSettings {
    ClaudeRcSettings {
        disable_remote_control: root
            .get("disableRemoteControl")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        remote_control_at_startup: root
            .get("remoteControlAtStartup")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        api_key_helper: root.get("apiKeyHelper").is_some_and(setting_value_present),
        env_auth_conflict: root
            .get("env")
            .and_then(Value::as_object)
            .is_some_and(|env| {
                env.get(ANTHROPIC_API_KEY)
                    .is_some_and(setting_value_present)
                    || env
                        .get(ANTHROPIC_AUTH_TOKEN)
                        .is_some_and(setting_value_present)
            }),
        env_endpoint_conflict: root
            .get("env")
            .and_then(Value::as_object)
            .is_some_and(endpoint_conflict_in),
    }
}

pub(crate) fn probed_version() -> Option<CliVersion> {
    probe_cli_version("claude")?.parse().ok()
}

pub(crate) fn pane_auto_enabled(settings: &ClaudeRcSettings, version: Option<CliVersion>) -> bool {
    if !settings.remote_control_at_startup || settings.disable_remote_control {
        return false;
    }
    let auth_conflict =
        settings.api_key_helper || settings.env_auth_conflict || launch_env_auth_conflict();
    if auth_conflict && version.is_some_and(|found| found >= AUTH_ENV_BLOCKS_RC_SINCE) {
        return false;
    }
    true
}

fn launch_env_auth_conflict() -> bool {
    env_value_present(ANTHROPIC_API_KEY) || env_value_present(ANTHROPIC_AUTH_TOKEN)
}

fn env_value_present(key: &str) -> bool {
    std::env::var_os(key).is_some_and(|value| !value.is_empty())
}

fn setting_value_present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(_) => true,
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(_) => true,
    }
}

pub(crate) fn launch_endpoint_conflict() -> bool {
    std::env::var(ANTHROPIC_BASE_URL)
        .ok()
        .is_some_and(|value| endpoint_is_conflicting(&value))
        || THIRD_PARTY_PROVIDER_VARS
            .iter()
            .any(|key| env_value_present(key))
}

fn endpoint_conflict_in(env: &Map<String, Value>) -> bool {
    env.get(ANTHROPIC_BASE_URL)
        .and_then(Value::as_str)
        .is_some_and(endpoint_is_conflicting)
        || THIRD_PARTY_PROVIDER_VARS
            .iter()
            .any(|key| env.get(*key).is_some_and(setting_value_present))
}

fn is_anthropic_api_url(value: &str) -> bool {
    matches!(
        value.trim().trim_end_matches('/'),
        "https://api.anthropic.com"
    )
}

fn endpoint_is_conflicting(value: &str) -> bool {
    !value.trim().is_empty() && !is_anthropic_api_url(value)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn settings(value: Value) -> ClaudeRcSettings {
        rc_settings_from(value.as_object().expect("object"))
    }

    #[test]
    fn rc_settings_from_reads_flags_conflicts_and_ignores_empties() {
        let parsed = settings(json!({
            "disableRemoteControl": true,
            "remoteControlAtStartup": true,
        }));
        assert!(parsed.disable_remote_control);
        assert!(parsed.remote_control_at_startup);

        // An apiKeyHelper or an env auth token (either key) is an auth conflict.
        assert!(settings(json!({ "apiKeyHelper": "op read key" })).api_key_helper);
        assert!(settings(json!({ "env": { "ANTHROPIC_API_KEY": "sk-ant" } })).env_auth_conflict);
        assert!(settings(json!({ "env": { "ANTHROPIC_AUTH_TOKEN": "token" } })).env_auth_conflict);
        assert!(
            settings(json!({ "env": { "ANTHROPIC_BASE_URL": "https://gateway.example" } }))
                .env_endpoint_conflict
        );
        assert!(
            settings(json!({ "env": { "CLAUDE_CODE_USE_BEDROCK": "1" } })).env_endpoint_conflict
        );
        assert!(
            !settings(json!({ "env": { "ANTHROPIC_BASE_URL": "https://api.anthropic.com/" } }))
                .env_endpoint_conflict
        );

        // Falsey, empty, and empty-env values read as the default (nothing set).
        assert_eq!(
            settings(json!({
                "disableRemoteControl": false,
                "remoteControlAtStartup": false,
                "apiKeyHelper": "",
                "env": { "ANTHROPIC_API_KEY": "" }
            })),
            ClaudeRcSettings::default()
        );
    }

    #[test]
    fn pane_auto_status_suppresses_settings_disabled_cases() {
        let disabled = ClaudeRcSettings {
            remote_control_at_startup: true,
            disable_remote_control: true,
            ..ClaudeRcSettings::default()
        };
        assert!(!pane_auto_enabled(&disabled, None));

        let off = ClaudeRcSettings {
            remote_control_at_startup: false,
            ..ClaudeRcSettings::default()
        };
        assert!(!pane_auto_enabled(&off, None));
    }

    #[test]
    fn pane_auto_status_uses_account_version_for_auth_gate() {
        let conflict = ClaudeRcSettings {
            remote_control_at_startup: true,
            api_key_helper: true,
            ..ClaudeRcSettings::default()
        };

        assert!(
            pane_auto_enabled(&conflict, None),
            "unknown versions apply only version-independent gates"
        );
        assert!(pane_auto_enabled(
            &conflict,
            Some(CliVersion::new(2, 1, 156))
        ));
        assert!(!pane_auto_enabled(
            &conflict,
            Some(CliVersion::new(2, 1, 157))
        ));
    }
}
