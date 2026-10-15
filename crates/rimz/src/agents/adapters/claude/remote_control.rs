//! Claude Code remote-control readiness, host argv, settings, and version gates.
//!
//! This module owns provider probing and setup guidance so room start, doctor,
//! daemon views, and runtime toggles consume one neutral readiness result.

use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::agents::runtime_control::{RuntimeControlIssue, RuntimeControlReadiness};
use crate::agents::version::{CliVersion, probe_cli_version};

use super::install::{claude_settings_path, read_existing_json};
use super::remote_consent;

pub(crate) const MIN_REMOTE_CONTROL: CliVersion = CliVersion::new(2, 1, 51);
pub(crate) const AUTH_ENV_BLOCKS_RC_SINCE: CliVersion = CliVersion::new(2, 1, 139);
pub(crate) const CUSTOM_ENDPOINT_BLOCKS_RC_SINCE: CliVersion = CliVersion::new(2, 1, 196);

const FALLBACK_SETTINGS_PATH: &str = "~/.claude/settings.json";
const ANTHROPIC_API_KEY: &str = "ANTHROPIC_API_KEY";
const ANTHROPIC_AUTH_TOKEN: &str = "ANTHROPIC_AUTH_TOKEN";
const CLAUDE_CODE_OAUTH_TOKEN: &str = "CLAUDE_CODE_OAUTH_TOKEN";
const ANTHROPIC_BASE_URL: &str = "ANTHROPIC_BASE_URL";
const TRAFFIC_SWITCHES: [&str; 2] = [
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
    "DISABLE_GROWTHBOOK",
];
const THIRD_PARTY_PROVIDER_VARS: [&str; 3] = [
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
];
const REMOTE_SESSION_ACCESS_TOKEN: &str = "CLAUDE_CODE_SESSION_ACCESS_TOKEN";
const ENVIRONMENT_KIND: &str = "CLAUDE_CODE_ENVIRONMENT_KIND";
const REMOTE_ENVIRONMENT_KIND: &str = "bridge";

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

fn remote_control_in_ancestry() -> bool {
    claude_host_in(crate::proc::ancestor_pids(), crate::proc::cmdline)
}

fn claude_host_in(pids: Vec<u32>, cmdline: impl Fn(u32) -> Option<String>) -> bool {
    pids.into_iter().any(|pid| {
        cmdline(pid).is_some_and(|command| crate::pane::command_is_claude_host(&command))
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ClaudeRcSettings {
    pub disable_remote_control: bool,
    pub remote_control_at_startup: bool,
    pub api_key_helper: bool,
    pub env_auth_conflict: bool,
    pub oauth_token_env: bool,
    pub env_endpoint_conflict: bool,
    pub env_traffic_switch: Option<&'static str>,
}

pub(crate) fn read_rc_settings(
    login_env: &BTreeMap<String, String>,
) -> (PathBuf, ClaudeRcSettings) {
    let path = settings_path(login_env);
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

pub(crate) fn settings_path(login_env: &BTreeMap<String, String>) -> PathBuf {
    match claude_settings_path(login_env) {
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
        env_traffic_switch: root.get("env").and_then(Value::as_object).and_then(|env| {
            TRAFFIC_SWITCHES
                .iter()
                .copied()
                .find(|key| env.get(*key).is_some_and(setting_value_present))
        }),
    }
}

pub(crate) fn probed_version() -> Option<CliVersion> {
    probe_cli_version("claude")?.parse().ok()
}

pub(crate) fn pane_auto_enabled(settings: &ClaudeRcSettings, version: Option<CliVersion>) -> bool {
    pane_auto_enabled_from(
        settings,
        version,
        env_value_present(ANTHROPIC_API_KEY),
        env_value_present(ANTHROPIC_AUTH_TOKEN),
        env_value_present(CLAUDE_CODE_OAUTH_TOKEN),
        launch_endpoint_conflict(),
        launch_traffic_switch(),
    )
}

fn pane_auto_enabled_from(
    settings: &ClaudeRcSettings,
    version: Option<CliVersion>,
    env_api_key: bool,
    env_auth_token: bool,
    env_oauth_token: bool,
    env_endpoint_conflict: bool,
    env_traffic_switch: Option<&str>,
) -> bool {
    settings.remote_control_at_startup
        && !settings.disable_remote_control
        && settings.env_traffic_switch.is_none()
        && env_traffic_switch.is_none()
        && auth_conflict_sources(
            version,
            settings,
            env_api_key,
            env_auth_token,
            env_oauth_token,
            env_endpoint_conflict,
        )
        .is_empty()
}

/// The authentication and endpoint sources Claude refuses Remote Control for
/// at `version`. A long-lived OAuth token is refused on every version; the
/// other rows apply only from their cutoff, so an unknown version skips them.
fn auth_conflict_sources(
    version: Option<CliVersion>,
    settings: &ClaudeRcSettings,
    env_api_key: bool,
    env_auth_token: bool,
    env_oauth_token: bool,
    env_endpoint_conflict: bool,
) -> Vec<AuthConflictSource> {
    let auth_gated = version.is_some_and(|found| found >= AUTH_ENV_BLOCKS_RC_SINCE);
    let endpoint_gated = version.is_some_and(|found| found >= CUSTOM_ENDPOINT_BLOCKS_RC_SINCE);
    [
        (auth_gated && env_api_key, AuthConflictSource::ApiKeyEnv),
        (
            auth_gated && env_auth_token,
            AuthConflictSource::AuthTokenEnv,
        ),
        (
            env_oauth_token || settings.oauth_token_env,
            AuthConflictSource::OAuthTokenEnv,
        ),
        (
            auth_gated && settings.api_key_helper,
            AuthConflictSource::ApiKeyHelperSetting,
        ),
        (
            auth_gated && settings.env_auth_conflict,
            AuthConflictSource::SettingsEnv,
        ),
        (
            endpoint_gated && env_endpoint_conflict,
            AuthConflictSource::EndpointEnv,
        ),
        (
            endpoint_gated && settings.env_endpoint_conflict,
            AuthConflictSource::SettingsEndpoint,
        ),
    ]
    .into_iter()
    .filter_map(|(applies, source)| applies.then_some(source))
    .collect()
}

fn env_value_present(key: &str) -> bool {
    std::env::var_os(key).is_some_and(|value| !value.is_empty())
}

fn launch_traffic_switch() -> Option<&'static str> {
    TRAFFIC_SWITCHES
        .iter()
        .copied()
        .find(|key| env_value_present(key))
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Issue {
    Uninstalled,
    TooOld {
        found: CliVersion,
    },
    RemoteControlDisabled {
        settings_path: PathBuf,
    },
    ConsentPending {
        config_path: PathBuf,
    },
    ConsentRefused {
        config_path: PathBuf,
    },
    AuthConflict {
        sources: Vec<AuthConflictSource>,
    },
    TrafficDisabled {
        variable: &'static str,
        settings_path: Option<PathBuf>,
    },
    FolderTrust {
        fix: String,
        command: Option<String>,
    },
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
        self.blocker(Path::new(FALLBACK_SETTINGS_PATH), &BTreeMap::new())
            .fmt(f)
    }
}

impl std::error::Error for Issue {}

impl Issue {
    /// The refusal with its fixes; a command that runs `claude` names the
    /// account home `login_env` carries, so it acts on the room's account.
    fn blocker(
        &self,
        settings_path: &Path,
        login_env: &BTreeMap<String, String>,
    ) -> crate::blocker::Blocker {
        use crate::blocker::{
            Blocker,
            Surface::{Plain, Privacy, Security},
        };
        let problem = |cause: String| {
            Blocker::new(format!(
                "Claude remote-control is enabled (`[remote_control] claude = true`) but {cause}"
            ))
        };
        let settings = settings_path.display();
        // The assignment prefix quotes only its value: a quoted whole word
        // would run as a command name rather than set the variable.
        let claude = |args: &[&str]| {
            let command =
                shlex::try_join(std::iter::once("claude").chain(args.iter().copied())).ok()?;
            match login_env.get("CLAUDE_CONFIG_DIR") {
                Some(home) => Some(format!(
                    "CLAUDE_CONFIG_DIR={} {command}",
                    shlex::try_quote(home).ok()?
                )),
                None => Some(command),
            }
        };
        match self {
            Self::Uninstalled => problem("`claude` is not on PATH.".to_owned()),
            Self::TooOld { found } => problem(format!(
                "`claude --version` reports {found}; \
                 remote control requires Claude Code >= {MIN_REMOTE_CONTROL}."
            ))
            .with_fix("Upgrade Claude Code", Some("claude update".into()), Plain),
            Self::RemoteControlDisabled { settings_path } => {
                let path = settings_path.display();
                problem(format!("`disableRemoteControl: true` in {path} blocks it.")).with_fix(
                    format!("Remove `disableRemoteControl` or set it to false in {path}"),
                    None,
                    Plain,
                )
            }
            Self::ConsentPending { config_path } => {
                let path = config_path.display();
                // RimZ replaces the file by a rename in its directory, so the
                // directory is what must be writable, not the file.
                let dir = config_path.parent().unwrap_or(Path::new("."));
                let chmod = shlex::try_join(["chmod", "u+w", &dir.to_string_lossy()]).ok();
                problem(format!(
                    "RimZ could not record `remoteDialogSeen` in {path}."
                ))
                .with_fix(
                    format!(
                        "Make {} writable so RimZ can record the dialog answer in {path}",
                        dir.display()
                    ),
                    chmod,
                    Plain,
                )
                .with_fix(
                    "Answer Claude's remote-control prompt once in this project",
                    claude(&["remote-control"]),
                    Plain,
                )
            }
            Self::ConsentRefused { config_path } => {
                let path = config_path.display();
                problem(format!("`remoteDialogSeen` is not true in {path}.")).with_fix(
                    format!("Set `remoteDialogSeen` to true in {path}"),
                    None,
                    Plain,
                )
            }
            Self::TrafficDisabled {
                variable,
                settings_path: Some(path),
            } => problem(format!(
                "`{variable}` in the settings env in {} blocks it.",
                path.display()
            ))
            .with_fix(
                format!("Remove `{variable}` from `env` in {}", path.display()),
                None,
                Privacy,
            ),
            Self::TrafficDisabled {
                variable,
                settings_path: None,
            } => problem(format!("`{variable}` in the launch environment blocks it.")).with_fix(
                "Unset the traffic switch in the launching shell",
                Some(format!("unset {variable}")),
                Privacy,
            ),
            Self::FolderTrust { fix, command } => problem(
                "the project has no folder-trust decision.".to_owned(),
            )
            .with_fix(fix, command.clone(), Security),
            Self::AuthConflict { sources } => {
                let mut blocker = problem(
                    "Claude Code disables remote control with the configured authentication \
                     or API endpoint on this version."
                        .to_owned(),
                );
                for source in sources {
                    let (summary, command) = match source {
                        AuthConflictSource::ApiKeyEnv => (
                            "Unset ANTHROPIC_API_KEY in the launching shell".to_owned(),
                            Some("unset ANTHROPIC_API_KEY".into()),
                        ),
                        AuthConflictSource::AuthTokenEnv => (
                            "Unset ANTHROPIC_AUTH_TOKEN in the launching shell".to_owned(),
                            Some("unset ANTHROPIC_AUTH_TOKEN".into()),
                        ),
                        AuthConflictSource::OAuthTokenEnv => (
                            format!(
                                "Unset CLAUDE_CODE_OAUTH_TOKEN in the launching shell \
                                 and remove it from `env` in {settings} if present"
                            ),
                            Some("unset CLAUDE_CODE_OAUTH_TOKEN".into()),
                        ),
                        AuthConflictSource::ApiKeyHelperSetting => {
                            (format!("Remove `apiKeyHelper` from {settings}"), None)
                        }
                        AuthConflictSource::SettingsEnv => (
                            format!(
                                "Remove ANTHROPIC_API_KEY and ANTHROPIC_AUTH_TOKEN \
                                 from `env` in {settings}"
                            ),
                            None,
                        ),
                        AuthConflictSource::EndpointEnv => (
                            "Unset custom endpoint and third-party provider values \
                             in the launching shell"
                                .to_owned(),
                            Some(
                                "unset ANTHROPIC_BASE_URL CLAUDE_CODE_USE_BEDROCK \
                                 CLAUDE_CODE_USE_VERTEX CLAUDE_CODE_USE_FOUNDRY"
                                    .into(),
                            ),
                        ),
                        AuthConflictSource::SettingsEndpoint => (
                            format!(
                                "Remove the custom Anthropic endpoint and third-party \
                                 provider values from `env` in {settings}"
                            ),
                            None,
                        ),
                    };
                    blocker = blocker.with_fix(summary, command, Security);
                }
                blocker.with_fix(
                    "Sign in to Claude again",
                    claude(&["auth", "login"]),
                    Security,
                )
            }
        }
    }
}

/// Probe configured Claude readiness once and return launch argv with success.
pub fn readiness(
    enabled: bool,
    login_env: &BTreeMap<String, String>,
    project_root: Option<&Path>,
) -> RuntimeControlReadiness {
    if !enabled {
        return RuntimeControlReadiness::Disabled;
    }
    if which::which("claude").is_err() {
        return RuntimeControlReadiness::Uninstalled(RuntimeControlIssue::new(
            "claude",
            "uninstalled",
            Issue::Uninstalled.blocker(Path::new(FALLBACK_SETTINGS_PATH), login_env),
        ));
    }
    let (settings_path, settings) = read_rc_settings(login_env);
    let version = (!settings.disable_remote_control)
        .then(probed_version)
        .flatten();
    match readiness_from(
        version,
        settings_path.clone(),
        settings,
        env_value_present(ANTHROPIC_API_KEY),
        env_value_present(ANTHROPIC_AUTH_TOKEN),
        env_value_present(CLAUDE_CODE_OAUTH_TOKEN),
        launch_endpoint_conflict(),
        remote_consent::read_consent(login_env),
        launch_traffic_switch(),
        project_root.map(|root| {
            let is_repo = crate::workspace::WorkspaceResolver::resolve(root, Some(root.into()))
                .map_or(true, |workspace| {
                    workspace.root_class == crate::workspace::RootClass::Repo
                });
            super::folder_trust::folder_trust(root, is_repo.then_some(root), login_env)
        }),
    ) {
        Ok(host_argv) => RuntimeControlReadiness::Ready {
            host_argv: Some(host_argv_with_login(host_argv, login_env)),
        },
        Err(issue) => RuntimeControlReadiness::Blocked(RuntimeControlIssue::new(
            "claude",
            "blocked",
            issue.blocker(&settings_path, login_env),
        )),
    }
}

fn host_argv_with_login(
    mut argv: Vec<String>,
    login_env: &BTreeMap<String, String>,
) -> Vec<String> {
    if let Some(home) = login_env.get("CLAUDE_CONFIG_DIR") {
        argv.splice(
            0..0,
            ["env".to_owned(), format!("CLAUDE_CONFIG_DIR={home}")],
        );
    }
    argv
}

/// Record the one-time remote-control dialog answer so an unattended host pane
/// starts serving instead of blocking on a prompt nobody is watching. The
/// config toggle is the operator's intent; this carries it to Claude. Failures
/// stay quiet here and surface as a refusal from [`readiness`], so a room never
/// launches a host that will hang.
pub fn ensure_consent(enabled: bool, login_env: &BTreeMap<String, String>) {
    if !enabled {
        return;
    }
    let Some(path) = remote_consent::global_config_path(login_env) else {
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
    env_traffic_switch: Option<&'static str>,
    folder_trust: Option<crate::agents::FolderTrust>,
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
    if let Some(variable) = env_traffic_switch {
        return Err(Issue::TrafficDisabled {
            variable,
            settings_path: None,
        });
    }
    if let Some(variable) = settings.env_traffic_switch {
        return Err(Issue::TrafficDisabled {
            variable,
            settings_path: Some(settings_path),
        });
    }
    if let Some(crate::agents::FolderTrust::Undecided(gap)) = folder_trust {
        return Err(Issue::FolderTrust {
            fix: if gap.grant.is_ok() {
                format!(
                    "Grant folder trust in {}, or answer Claude's folder-trust prompt there",
                    gap.key.display()
                )
            } else {
                gap.fix("claude")
            },
            command: gap
                .grant
                .is_ok()
                .then(|| "rimz trust grant --agents claude".to_owned()),
        });
    }
    match version {
        Some(found) if found < MIN_REMOTE_CONTROL => return Err(Issue::TooOld { found }),
        Some(_) => {}
        None => tracing::warn!(
            "Claude remote-control preflight could not determine `claude --version`; applying version-independent gates only"
        ),
    }
    let sources = auth_conflict_sources(
        version,
        &settings,
        env_api_key,
        env_auth_token,
        env_oauth_token,
        env_endpoint_conflict,
    );
    if sources.is_empty() {
        Ok(host_argv())
    } else {
        Err(Issue::AuthConflict { sources })
    }
}

#[cfg(test)]
mod tests;
