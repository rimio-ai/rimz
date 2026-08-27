//! Claude Code remote-control readiness, host argv, settings, and version gates.
//!
//! This module owns provider probing and setup guidance so room start, doctor,
//! daemon views, and runtime toggles consume one native readiness result.

use serde_json::{Map, Value};
use std::ffi::OsStr;
use std::path::PathBuf;

use crate::agents::version::{CliVersion, probe_cli_version};

use super::install::{claude_settings_path, read_existing_json};
use super::remote_consent;

pub(crate) const MIN_REMOTE_CONTROL: CliVersion = CliVersion::new(2, 1, 51);
pub(crate) const AUTH_ENV_BLOCKS_RC_SINCE: CliVersion = CliVersion::new(2, 1, 157);
pub(crate) const CUSTOM_ENDPOINT_BLOCKS_RC_SINCE: CliVersion = CliVersion::new(2, 1, 196);

const FALLBACK_SETTINGS_PATH: &str = "~/.claude/settings.json";
const ANTHROPIC_API_KEY: &str = "ANTHROPIC_API_KEY";
const ANTHROPIC_AUTH_TOKEN: &str = "ANTHROPIC_AUTH_TOKEN";
const CLAUDE_CODE_OAUTH_TOKEN: &str = "CLAUDE_CODE_OAUTH_TOKEN";
const ANTHROPIC_BASE_URL: &str = "ANTHROPIC_BASE_URL";
const THIRD_PARTY_PROVIDER_VARS: [&str; 3] = [
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
];
const REMOTE_SESSION_ACCESS_TOKEN: &str = "CLAUDE_CODE_SESSION_ACCESS_TOKEN";
const ENVIRONMENT_KIND: &str = "CLAUDE_CODE_ENVIRONMENT_KIND";
const REMOTE_ENVIRONMENT_KIND: &str = "bridge";
const MAX_ANCESTOR_DEPTH: usize = 32;

/// Whether this process was spawned as a Claude remote-control session hook.
/// Environment markers are the fast path; the bounded ancestry walk covers
/// upstream versions that omit them.
pub fn spawned_by_remote_control() -> bool {
    let access_token = std::env::var_os(REMOTE_SESSION_ACCESS_TOKEN);
    let environment_kind = std::env::var_os(ENVIRONMENT_KIND);
    if remote_session_env(access_token.as_deref(), environment_kind.as_deref()) {
        return true;
    }
    remote_control_in_ancestry()
}

fn remote_session_env(access_token: Option<&OsStr>, environment_kind: Option<&OsStr>) -> bool {
    access_token.is_some_and(|value| !value.is_empty())
        || environment_kind.is_some_and(|value| value == OsStr::new(REMOTE_ENVIRONMENT_KIND))
}

#[cfg(unix)]
fn remote_control_in_ancestry() -> bool {
    remote_control_in_ancestry_from(
        std::os::unix::process::parent_id(),
        |pid| crate::proc::comm_and_ppid(pid).map(|(_, ppid)| ppid),
        crate::proc::cmdline,
    )
}

#[cfg(not(unix))]
fn remote_control_in_ancestry() -> bool {
    false
}

fn remote_control_in_ancestry_from(
    mut pid: u32,
    mut parent_pid: impl FnMut(u32) -> Option<u32>,
    mut cmdline: impl FnMut(u32) -> Option<String>,
) -> bool {
    for _ in 0..MAX_ANCESTOR_DEPTH {
        if pid <= 1 {
            return false;
        }
        if cmdline(pid).is_some_and(|command| crate::daemon_view::command_is_claude_host(&command))
        {
            return true;
        }
        let Some(parent) = parent_pid(pid) else {
            return false;
        };
        pid = parent;
    }
    false
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ClaudeRcSettings {
    pub disable_remote_control: bool,
    pub remote_control_at_startup: bool,
    pub api_key_helper: bool,
    pub env_auth_conflict: bool,
    pub oauth_token_env: bool,
    pub env_endpoint_conflict: bool,
}

pub(crate) fn read_rc_settings() -> (PathBuf, ClaudeRcSettings) {
    let path = settings_path();
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

pub(crate) fn settings_path() -> PathBuf {
    match claude_settings_path() {
        Ok(path) => path,
        Err(err) => {
            tracing::warn!(error = %err, "Claude settings path unavailable for remote-control read");
            PathBuf::from(FALLBACK_SETTINGS_PATH)
        }
    }
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
        oauth_token_env: root
            .get("env")
            .and_then(Value::as_object)
            .is_some_and(|env| {
                env.get(CLAUDE_CODE_OAUTH_TOKEN)
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
    let auth_conflict = settings.api_key_helper
        || settings.env_auth_conflict
        || settings.oauth_token_env
        || launch_env_auth_conflict();
    if auth_conflict && version.is_some_and(|found| found >= AUTH_ENV_BLOCKS_RC_SINCE) {
        return false;
    }
    true
}

fn launch_env_auth_conflict() -> bool {
    env_value_present(ANTHROPIC_API_KEY)
        || env_value_present(ANTHROPIC_AUTH_TOKEN)
        || env_value_present(CLAUDE_CODE_OAUTH_TOKEN)
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

/// Claude-native readiness plus the host argv when launchable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Readiness {
    Disabled,
    Ready { host_argv: Vec<String> },
    Uninstalled(Issue),
    Blocked(Issue),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Issue {
    Uninstalled,
    TooOld { found: CliVersion },
    RemoteControlDisabled { settings_path: PathBuf },
    ConsentPending { config_path: PathBuf },
    ConsentRefused { config_path: PathBuf },
    AuthConflict { sources: Vec<AuthConflictSource> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthConflictSource {
    ApiKeyEnv,
    AuthTokenEnv,
    OAuthTokenEnv,
    ApiKeyHelperSetting,
    SettingsEnv,
    EndpointEnv,
    SettingsEndpoint,
}

impl std::fmt::Display for AuthConflictSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ApiKeyEnv => write!(f, "ANTHROPIC_API_KEY in the launch environment"),
            Self::AuthTokenEnv => write!(f, "ANTHROPIC_AUTH_TOKEN in the launch environment"),
            Self::OAuthTokenEnv => write!(
                f,
                "CLAUDE_CODE_OAUTH_TOKEN in the launch environment or Claude settings env"
            ),
            Self::ApiKeyHelperSetting => write!(f, "apiKeyHelper in Claude settings"),
            Self::SettingsEnv => write!(
                f,
                "ANTHROPIC_API_KEY/ANTHROPIC_AUTH_TOKEN in Claude settings env"
            ),
            Self::EndpointEnv => write!(
                f,
                "a custom Anthropic endpoint or third-party provider in the launch environment"
            ),
            Self::SettingsEndpoint => write!(
                f,
                "a custom Anthropic endpoint or third-party provider in Claude settings env"
            ),
        }
    }
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Uninstalled => write!(
                f,
                "Claude remote-control is enabled (`[remote_control] claude = true`) but `claude` is not on PATH."
            ),
            Self::TooOld { found } => write!(
                f,
                "Claude remote-control is enabled (`[remote_control] claude = true`) but \
                 `claude --version` reports {found}; remote control requires Claude Code \
                 >= {MIN_REMOTE_CONTROL}.\n\n\
                 Upgrade Claude Code, then re-run, or set `[remote_control] claude = false` \
                 to disable the Claude host."
            ),
            Self::RemoteControlDisabled { settings_path } => write!(
                f,
                "Claude remote-control is enabled (`[remote_control] claude = true`) but \
                 `disableRemoteControl: true` in {} blocks it.\n\n\
                 Remove that setting or set it to false, then re-run, or set \
                 `[remote_control] claude = false` to disable the Claude host.",
                settings_path.display(),
            ),
            Self::ConsentPending { config_path } => write!(
                f,
                "Claude remote-control is enabled (`[remote_control] claude = true`) but \
                 `claude remote-control` still asks `Enable Remote Control? (y/n)` once per \
                 machine, and RimZ could not record the answer in {}.\n\n\
                 Make that file writable and re-run so RimZ can set `remoteDialogSeen`, or run \
                 `claude remote-control` in this project once by hand and answer `y`, or set \
                 `[remote_control] claude = false` to disable the Claude host.",
                config_path.display(),
            ),
            Self::ConsentRefused { config_path } => write!(
                f,
                "Claude remote-control is enabled (`[remote_control] claude = true`) but \
                 `remoteDialogSeen` is set to a non-`true` value in {}, which RimZ leaves \
                 alone.\n\n\
                 Set it to `true` or remove it, then re-run, or set \
                 `[remote_control] claude = false` to disable the Claude host.",
                config_path.display(),
            ),
            Self::AuthConflict { sources } => write!(
                f,
                "Claude remote-control is enabled (`[remote_control] claude = true`) but \
                 Claude Code disables remote control with the configured authentication \
                 or API endpoint on this version. Conflicting source(s): {}.\n\n\
                 Unset those environment values or remove them from Claude settings, run \
                 `claude auth login`, then re-run; or set `[remote_control] claude = false` \
                 to disable the Claude host.",
                sources
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        }
    }
}

impl std::error::Error for Issue {}

/// Probe configured Claude readiness once and return launch argv with success.
pub fn readiness(enabled: bool) -> Readiness {
    if !enabled {
        return Readiness::Disabled;
    }
    if which::which("claude").is_err() {
        return Readiness::Uninstalled(Issue::Uninstalled);
    }
    let (settings_path, settings) = read_rc_settings();
    let version = (!settings.disable_remote_control)
        .then(probed_version)
        .flatten();
    match readiness_from(
        version,
        settings_path,
        settings,
        env_value_present(ANTHROPIC_API_KEY),
        env_value_present(ANTHROPIC_AUTH_TOKEN),
        env_value_present(CLAUDE_CODE_OAUTH_TOKEN),
        launch_endpoint_conflict(),
        remote_consent::read_consent(),
    ) {
        Ok(host_argv) => Readiness::Ready { host_argv },
        Err(issue) => Readiness::Blocked(issue),
    }
}

/// Record the one-time remote-control dialog answer so an unattended host pane
/// starts serving instead of blocking on a prompt nobody is watching. The
/// config toggle is the operator's intent; this carries it to Claude. Failures
/// stay quiet here and surface as a refusal from [`readiness`], so a room never
/// launches a host that will hang.
pub fn ensure_consent(enabled: bool) {
    if !enabled {
        return;
    }
    let Some(path) = remote_consent::global_config_path() else {
        tracing::warn!(
            "Claude global config path unavailable; remote-control consent was not recorded",
        );
        return;
    };
    match remote_consent::seed(&path) {
        Ok(remote_consent::ConsentState::Seeded) => {}
        Ok(state) => tracing::warn!(
            path = %path.display(),
            ?state,
            "Claude remote-control consent was not recorded",
        ),
        Err(err) => tracing::warn!(
            path = %path.display(),
            error = &err as &dyn std::error::Error,
            "Claude remote-control consent could not be written",
        ),
    }
}

/// Build the exact workspace-scoped foreground host command.
pub fn host_argv() -> Vec<String> {
    vec![
        "claude".to_owned(),
        "remote-control".to_owned(),
        "--spawn".to_owned(),
        "worktree".to_owned(),
    ]
}

#[allow(clippy::too_many_arguments)]
fn readiness_from(
    version: Option<CliVersion>,
    settings_path: PathBuf,
    settings: ClaudeRcSettings,
    env_api_key: bool,
    env_auth_token: bool,
    env_oauth_token: bool,
    env_endpoint_conflict: bool,
    consent: Option<(PathBuf, remote_consent::ConsentState)>,
) -> Result<Vec<String>, Issue> {
    if settings.disable_remote_control {
        return Err(Issue::RemoteControlDisabled { settings_path });
    }
    // The dialog blocks the host before any version or auth gate matters: an
    // unanswered prompt holds the pane open forever. An unreadable global config
    // stays silent here — Claude owns that file and may still recover it.
    if let Some((config_path, state)) = consent {
        match state {
            remote_consent::ConsentState::Seeded | remote_consent::ConsentState::Unreadable => {}
            remote_consent::ConsentState::Unseeded => {
                return Err(Issue::ConsentPending { config_path });
            }
            remote_consent::ConsentState::Refused => {
                return Err(Issue::ConsentRefused { config_path });
            }
        }
    }
    let Some(found) = version else {
        tracing::warn!(
            "Claude remote-control preflight could not determine `claude --version`; applying version-independent gates only"
        );
        return Ok(host_argv());
    };
    if found < MIN_REMOTE_CONTROL {
        return Err(Issue::TooOld { found });
    }

    let mut sources = Vec::new();
    if env_api_key {
        sources.push(AuthConflictSource::ApiKeyEnv);
    }
    if env_auth_token {
        sources.push(AuthConflictSource::AuthTokenEnv);
    }
    if env_oauth_token || settings.oauth_token_env {
        sources.push(AuthConflictSource::OAuthTokenEnv);
    }
    if settings.api_key_helper {
        sources.push(AuthConflictSource::ApiKeyHelperSetting);
    }
    if settings.env_auth_conflict {
        sources.push(AuthConflictSource::SettingsEnv);
    }
    if found < AUTH_ENV_BLOCKS_RC_SINCE {
        sources.clear();
    }
    if found >= CUSTOM_ENDPOINT_BLOCKS_RC_SINCE {
        if env_endpoint_conflict {
            sources.push(AuthConflictSource::EndpointEnv);
        }
        if settings.env_endpoint_conflict {
            sources.push(AuthConflictSource::SettingsEndpoint);
        }
    }
    if sources.is_empty() {
        Ok(host_argv())
    } else {
        Err(Issue::AuthConflict { sources })
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use serde_json::json;

    use super::*;

    fn settings(value: Value) -> ClaudeRcSettings {
        rc_settings_from(value.as_object().expect("object"))
    }

    #[test]
    fn remote_session_env_accepts_each_upstream_marker() {
        assert!(remote_session_env(Some(OsStr::new("token")), None));
        assert!(remote_session_env(None, Some(OsStr::new("bridge"))));
        assert!(!remote_session_env(Some(OsStr::new("")), None));
        assert!(!remote_session_env(None, Some(OsStr::new("local"))));
        assert!(!remote_session_env(None, None));
    }

    #[test]
    fn remote_session_ancestry_matches_only_a_claude_remote_control_host() {
        let parent = |pid| match pid {
            30 => Some(20),
            20 => Some(10),
            10 => Some(1),
            _ => None,
        };
        let remote = |pid| match pid {
            30 => Some("/versions/claude --print --sdk-url wss://example".to_owned()),
            20 => Some("/usr/local/bin/claude remote-control --spawn worktree".to_owned()),
            _ => None,
        };
        assert!(remote_control_in_ancestry_from(30, parent, remote));

        let normal = |pid| match pid {
            30 => Some("/usr/local/bin/claude --resume session".to_owned()),
            20 => Some("zsh".to_owned()),
            _ => None,
        };
        assert!(!remote_control_in_ancestry_from(30, parent, normal));
    }

    #[test]
    fn remote_session_ancestry_walk_is_bounded() {
        let calls = Cell::new(0);
        assert!(!remote_control_in_ancestry_from(
            100,
            |pid| {
                calls.set(calls.get() + 1);
                Some(pid + 1)
            },
            |_| None,
        ));
        assert_eq!(calls.get(), MAX_ANCESTOR_DEPTH);
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
        assert!(settings(json!({ "env": { "CLAUDE_CODE_OAUTH_TOKEN": "token" } })).oauth_token_env);
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
                "env": {
                    "ANTHROPIC_API_KEY": "",
                    "CLAUDE_CODE_OAUTH_TOKEN": ""
                }
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

    fn readiness_settings() -> ClaudeRcSettings {
        ClaudeRcSettings::default()
    }

    fn settings_path() -> PathBuf {
        PathBuf::from("/home/u/.claude/settings.json")
    }

    fn v(patch: u64) -> Option<CliVersion> {
        Some(CliVersion::new(2, 1, patch))
    }

    fn consent(
        state: remote_consent::ConsentState,
    ) -> Option<(PathBuf, remote_consent::ConsentState)> {
        Some((PathBuf::from("/home/u/.claude.json"), state))
    }

    fn decision(
        version: Option<CliVersion>,
        settings: ClaudeRcSettings,
        env_api_key: bool,
        env_auth_token: bool,
        env_oauth_token: bool,
    ) -> Result<Vec<String>, Issue> {
        readiness_from(
            version,
            settings_path(),
            settings,
            env_api_key,
            env_auth_token,
            env_oauth_token,
            false,
            consent(remote_consent::ConsentState::Seeded),
        )
    }

    #[test]
    fn host_argv_uses_worktree_spawn() {
        let argv = host_argv();
        assert_eq!(
            argv,
            vec!["claude", "remote-control", "--spawn", "worktree"]
        );
        assert!(crate::daemon_view::command_is_host(&argv.join(" ")));
    }

    #[test]
    fn an_unrecorded_dialog_blocks_before_any_version_or_auth_gate() {
        // An unanswered prompt holds the pane open, so it outranks every gate
        // that only degrades the host.
        let blocked = readiness_from(
            None,
            settings_path(),
            readiness_settings(),
            true,
            true,
            true,
            true,
            consent(remote_consent::ConsentState::Unseeded),
        );
        assert_eq!(
            blocked,
            Err(Issue::ConsentPending {
                config_path: PathBuf::from("/home/u/.claude.json"),
            })
        );
    }

    #[test]
    fn an_explicit_refusal_is_reported_rather_than_overridden() {
        assert_eq!(
            readiness_from(
                v(215),
                settings_path(),
                readiness_settings(),
                false,
                false,
                false,
                false,
                consent(remote_consent::ConsentState::Refused),
            ),
            Err(Issue::ConsentRefused {
                config_path: PathBuf::from("/home/u/.claude.json"),
            })
        );
    }

    #[test]
    fn an_unreadable_global_config_leaves_the_host_launchable() {
        // Claude owns that file and may still recover it; refusing the room
        // over a file RimZ cannot parse would be worse than letting it try.
        assert!(
            readiness_from(
                v(215),
                settings_path(),
                readiness_settings(),
                false,
                false,
                false,
                false,
                consent(remote_consent::ConsentState::Unreadable),
            )
            .is_ok()
        );
    }

    #[test]
    fn consent_issues_keep_claude_setup_guidance() {
        let pending = Issue::ConsentPending {
            config_path: PathBuf::from("/home/u/.claude.json"),
        }
        .to_string();
        assert!(pending.contains("[remote_control] claude"));
        assert!(pending.contains("remoteDialogSeen"));
        assert!(pending.contains("/home/u/.claude.json"));

        let refused = Issue::ConsentRefused {
            config_path: PathBuf::from("/home/u/.claude.json"),
        }
        .to_string();
        assert!(refused.contains("[remote_control] claude"));
        assert!(refused.contains("remoteDialogSeen"));
    }

    #[test]
    fn issues_keep_claude_setup_guidance() {
        let issue = Issue::TooOld {
            found: CliVersion::new(2, 1, 50),
        }
        .to_string();
        assert!(issue.contains("[remote_control] claude"));
        assert!(issue.contains(">= 2.1.51"));
    }

    #[test]
    fn readiness_blocks_old_versions_and_disabled_settings() {
        assert_eq!(
            decision(v(50), readiness_settings(), false, false, false),
            Err(Issue::TooOld {
                found: CliVersion::new(2, 1, 50)
            })
        );

        let settings = ClaudeRcSettings {
            disable_remote_control: true,
            ..readiness_settings()
        };
        assert_eq!(
            decision(v(173), settings, false, false, false),
            Err(Issue::RemoteControlDisabled {
                settings_path: settings_path()
            })
        );
    }

    #[test]
    fn readiness_auth_conflict_gate_starts_at_2_1_157() {
        let settings = ClaudeRcSettings {
            api_key_helper: true,
            env_auth_conflict: true,
            ..readiness_settings()
        };
        assert!(decision(v(156), settings.clone(), true, true, true).is_ok());
        assert_eq!(
            decision(v(157), settings, true, true, true),
            Err(Issue::AuthConflict {
                sources: vec![
                    AuthConflictSource::ApiKeyEnv,
                    AuthConflictSource::AuthTokenEnv,
                    AuthConflictSource::OAuthTokenEnv,
                    AuthConflictSource::ApiKeyHelperSetting,
                    AuthConflictSource::SettingsEnv,
                ]
            })
        );
    }

    #[test]
    fn readiness_blocks_long_lived_oauth_tokens_from_env_or_settings() {
        assert_eq!(
            decision(v(247), readiness_settings(), false, false, true),
            Err(Issue::AuthConflict {
                sources: vec![AuthConflictSource::OAuthTokenEnv],
            })
        );

        let settings = ClaudeRcSettings {
            oauth_token_env: true,
            ..readiness_settings()
        };
        assert_eq!(
            decision(v(247), settings, false, false, false),
            Err(Issue::AuthConflict {
                sources: vec![AuthConflictSource::OAuthTokenEnv],
            })
        );

        let guidance = Issue::AuthConflict {
            sources: vec![AuthConflictSource::OAuthTokenEnv],
        }
        .to_string();
        assert!(guidance.contains("CLAUDE_CODE_OAUTH_TOKEN"));
        assert!(guidance.contains("claude auth login"));
    }

    #[test]
    fn readiness_custom_endpoint_gate_starts_at_2_1_196() {
        let settings = ClaudeRcSettings {
            env_endpoint_conflict: true,
            ..readiness_settings()
        };
        assert!(
            readiness_from(
                v(195),
                settings_path(),
                settings.clone(),
                false,
                false,
                false,
                true,
                consent(remote_consent::ConsentState::Seeded),
            )
            .is_ok()
        );
        assert_eq!(
            readiness_from(
                v(196),
                settings_path(),
                settings,
                false,
                false,
                false,
                true,
                consent(remote_consent::ConsentState::Seeded),
            ),
            Err(Issue::AuthConflict {
                sources: vec![
                    AuthConflictSource::EndpointEnv,
                    AuthConflictSource::SettingsEndpoint,
                ]
            })
        );
    }

    #[test]
    fn unknown_version_applies_only_settings_independent_gate() {
        let settings = ClaudeRcSettings {
            api_key_helper: true,
            ..readiness_settings()
        };
        assert!(decision(None, settings, true, false, true).is_ok());

        let settings = ClaudeRcSettings {
            disable_remote_control: true,
            ..readiness_settings()
        };
        assert_eq!(
            decision(None, settings, false, false, false),
            Err(Issue::RemoteControlDisabled {
                settings_path: settings_path()
            })
        );
    }
}
