//! Provider-process compilation, exec-wrapper argv, and login-shell launch policy.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agents::capabilities::SystemTextChannel;
use crate::disk::paths::{RuntimePaths, StatePaths};
use crate::harness::launch_reminders::LaunchReminders;
use crate::harness::prompt_compose::{TEXT_PROMPT_LIMIT, prompt_artifact_path};
use crate::harness::spec::AgentCell;
use crate::ids::{AgentKind, MessageId, RunId};

const ENV_BIN: &str = "/usr/bin/env";
const POSIX_LOGIN_SHELL_SCRIPT: &str = r#"exec /usr/bin/env "$@""#;
const FISH_LOGIN_SHELL_SCRIPT: &str = "exec /usr/bin/env $argv";
const POSIX_ARG0: &str = "rimz-agent-launch";

pub const ENV_RUN_ID: &str = "RIMZ_RUN_ID";
/// Launch reminders for an adapter whose RimZ extension injects them
/// (`SystemTextChannel::ExtensionEnv`). Always exported, empty when there is no
/// reminder, so a parent's value never leaks into a child launch.
const ENV_LAUNCH_REMINDERS: &str = "RIMZ_LAUNCH_REMINDERS";
/// `1` when RimZ's extension requests priority serving for this launch;
/// cleared on every other launch so a nested launch never inherits it.
const ENV_LAUNCH_FAST: &str = "RIMZ_LAUNCH_FAST";
/// `1` when the integration replaces native auto-approval for this frontend.
const ENV_LAUNCH_AUTO_APPROVE: &str = "RIMZ_LAUNCH_AUTO_APPROVE";
/// The exact `KEY=VALUE` pair this launch set for long prompt-cache retention.
const ENV_LONG_CACHE_ENV: &str = "RIMZ_LONG_CACHE_ENV";
/// The exact `KEY=VALUE` pair this launch set for the provider's code mode.
const ENV_CODE_MODE_ENV: &str = "RIMZ_CODE_MODE_ENV";
/// The config member (`CodeModeSwitch::Config`'s marker) a launch wrote for the provider's
/// code mode. Only a launch of the kind that declares the marker clears it, so it outlives
/// launches of other kinds in between.
const ENV_CODE_MODE_CONFIG: &str = "RIMZ_CODE_MODE_CONFIG";
/// Stable RimZ launch identity. The provider may replace the provisional
/// session key after startup, so launch ancestry resolves this value through
/// the durable `AgentState::launch_id` stamp.
pub const ENV_AGENT_ID: &str = "RIMZ_AGENT_ID";
/// The launched adapter kind (`claude`, `codex`, ...). Its presence marks the
/// process as a RimZ-launched agent for peer-message attribution.
pub const ENV_AGENT_KIND: &str = "RIMZ_AGENT_KIND";
pub const ENV_AGENT_NAME: &str = "RIMZ_AGENT_NAME";
/// The `[agents.profiles]` profile name an agent launched as, so it answers to
/// `@<profile>`. Set by the launch wrapper; read into the lifecycle observation.
pub const ENV_AGENT_PROFILE: &str = "RIMZ_AGENT_PROFILE";
/// The `[agents.teams]` role name an agent launched as, so it answers to
/// `@<role>`. Set by the launch wrapper; read into the lifecycle observation.
pub const ENV_AGENT_ROLE: &str = "RIMZ_AGENT_ROLE";
/// The team name an agent launched under. Set by the launch wrapper, or
/// cleared when the launch carries no team identity.
pub const ENV_TEAM: &str = "RIMZ_TEAM";
/// `1` on a launch whose volatile Environment facts arrive at prompt submit;
/// cleared on every other launch so a nested launch never inherits it. Read by
/// the prompt-submit hook.
pub const ENV_RUNTIME_ENV: &str = "RIMZ_RUNTIME_ENV";
/// The inline multi-agent launch cohort this agent belongs to. Team launches
/// use [`ENV_TEAM`] as their cohort key; inline layouts use this generated id.
pub const ENV_LAUNCH_GROUP: &str = "RIMZ_LAUNCH_GROUP";
/// The agent's order inside its launch cohort: team role-list index or inline
/// agent-cell index. Set by the wrapper; read into lifecycle observations.
pub const ENV_LAUNCH_ORDINAL: &str = "RIMZ_LAUNCH_ORDINAL";
/// The model selected by launch flags or profile presets. Set by the launch
/// wrapper; read into lifecycle observations only while the session's rollup
/// has no model.
pub const ENV_AGENT_MODEL: &str = "RIMZ_AGENT_MODEL";
/// The reasoning effort selected by launch flags or profile presets. Set by
/// the launch wrapper; read into lifecycle observations only while the
/// session's rollup has no effort.
pub const ENV_AGENT_EFFORT: &str = "RIMZ_AGENT_EFFORT";
/// The canonical dollar cap selected by launch flags, profiles, or roles.
/// Set by the launch wrapper and read into lifecycle observations.
pub(crate) const ENV_AGENT_BUDGET: &str = "RIMZ_AGENT_BUDGET";
/// The count of RimZ swaps of this agent the process runs under. Every launch
/// writes it, `0` when it carries no epoch, so a child never inherits its
/// caller's; the hook reads it back with [`ENV_AGENT_ID`] as its event stamp.
pub const ENV_AGENT_EPOCH: &str = "RIMZ_AGENT_EPOCH";
/// Claude Code's own shell override, stamped beside `SHELL` under a configured
/// `[agents] shell` so the provider's tool shell is the wrapper's.
const ENV_CLAUDE_CODE_SHELL: &str = "CLAUDE_CODE_SHELL";
const AGENT_SHELL_FIX: &str = "set it to the name or full path of an installed bash, zsh, fish or sh-family shell, or remove the line from the per-machine config";

/// Whether a room birth saves its bare-name shell lookup for later launches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentShellPersistence {
    Room,
    PerLaunch,
}

/// A configured `[agents] shell` that cannot carry an agent launch.
#[derive(Clone, Debug, thiserror::Error)]
#[error("[agents] shell = \"{}\" {reason}; {AGENT_SHELL_FIX}", shell.display())]
pub struct AgentShellErr {
    shell: PathBuf,
    reason: String,
    path: Option<PathBuf>,
}

impl From<AgentShellErr> for crate::blocker::Blocker {
    fn from(error: AgentShellErr) -> Self {
        Self::new(format!(
            "[agents] shell = \"{}\" {}",
            error.shell.display(),
            error.reason
        ))
        .with_fix(
            AGENT_SHELL_FIX,
            Some("rimz config set agents.shell bash".into()),
            crate::blocker::Surface::Plain,
        )
    }
}

fn validate_agent_shell(shell: &Path) -> Result<(), AgentShellErr> {
    let reason = if !shell.is_absolute() {
        "is not an absolute path"
    } else if !shell.is_file() {
        "does not exist"
    } else if !crate::proc::launchable_shell(shell) {
        "disables logins"
    } else if ShellFamily::from_shell(shell) == ShellFamily::Csh {
        "is a csh-family shell"
    } else {
        return Ok(());
    };
    Err(AgentShellErr {
        shell: shell.to_path_buf(),
        reason: reason.into(),
        path: Some(shell.to_path_buf()),
    })
}

/// The configured shell's room-aware selection, including a failed lookup or validation.
#[derive(Clone, Debug)]
pub struct AgentShellResolution {
    pub name: PathBuf,
    pub path: Result<PathBuf, AgentShellErr>,
    pub saved: bool,
    pub(super) missing_saved_path: Option<PathBuf>,
}

impl AgentShellResolution {
    pub fn is_bare_name(&self) -> bool {
        bare_agent_shell(&self.name)
    }

    /// The selected path even when validation fails; a PATH miss selects nothing.
    pub fn selected_path(&self) -> Option<&Path> {
        match &self.path {
            Ok(path) => Some(path),
            Err(error) => error.path.as_deref(),
        }
    }

    pub fn missing_saved_path(&self) -> Option<&Path> {
        self.missing_saved_path.as_deref()
    }
}

/// Resolve a configured shell through a matching room lookup, then this process's PATH.
/// With no record path, take a fresh lookup suitable for birth or configuration editing.
pub fn resolve_agent_shell(shell: &Path, record_path: Option<&Path>) -> AgentShellResolution {
    let saved = record_path
        .filter(|_| bare_agent_shell(shell))
        .and_then(|path| crate::workspace::record::read_optional(path).ok().flatten())
        .and_then(|record| record.agent_shell);
    let path = std::env::var_os("PATH");
    let cwd = std::env::current_dir().ok();
    resolve_agent_shell_with(shell, saved.as_ref(), path.as_deref(), cwd.as_deref())
}

fn bare_agent_shell(shell: &Path) -> bool {
    shell
        .file_name()
        .is_some_and(|name| shell.as_os_str() == name)
}

fn resolve_agent_shell_with(
    shell: &Path,
    saved: Option<&crate::workspace::record::AgentShell>,
    path: Option<&std::ffi::OsStr>,
    cwd: Option<&Path>,
) -> AgentShellResolution {
    let saved = saved.filter(|saved| bare_agent_shell(shell) && saved.name == shell);
    let (saved, missing_saved_path) = match saved {
        Some(saved) if !saved.path.is_file() => (None, Some(saved.path.clone())),
        saved => (saved, None),
    };
    let path = if shell.is_absolute() {
        validate_agent_shell(shell).map(|()| shell.to_path_buf())
    } else if !bare_agent_shell(shell) {
        Err(AgentShellErr {
            shell: shell.to_path_buf(),
            reason: "is a relative path".into(),
            path: None,
        })
    } else if let Some(saved) = saved {
        validate_agent_shell(&saved.path)
            .map(|()| saved.path.clone())
            .map_err(|error| AgentShellErr {
                shell: shell.to_path_buf(),
                ..error
            })
    } else {
        let hit = path.and_then(|path| {
            let mut lookup = which::WhichConfig::new()
                .binary_name(shell.as_os_str().to_owned())
                .custom_path_list(path.to_owned())
                .system_cwd(false);
            if let Some(cwd) = cwd {
                lookup = lookup.custom_cwd(cwd.to_path_buf());
            }
            lookup.first_result().ok()
        });
        hit.ok_or_else(|| AgentShellErr {
            shell: shell.to_path_buf(),
            reason: "is not on PATH".into(),
            path: None,
        })
        .and_then(|path| {
            validate_agent_shell(&path)
                .map(|()| path)
                .map_err(|error| AgentShellErr {
                    shell: shell.to_path_buf(),
                    ..error
                })
        })
    };
    AgentShellResolution {
        name: shell.to_path_buf(),
        path,
        saved: saved.is_some(),
        missing_saved_path,
    }
}

/// The env a configured `[agents] shell` stamps on a launch: the shell under
/// both names the provider reads, and the user's own shell for shell panes a
/// nested `rimz` opens.
fn agent_shell_env(shell: &Path, user_shell: Option<&Path>) -> BTreeMap<String, String> {
    let shell = shell.display().to_string();
    let mut env = BTreeMap::from([
        ("SHELL".to_owned(), shell.clone()),
        (ENV_CLAUDE_CODE_SHELL.to_owned(), shell),
    ]);
    if let Some(user) = user_shell {
        env.insert(
            crate::proc::USER_SHELL_ENV.to_owned(),
            user.display().to_string(),
        );
    }
    env
}

#[derive(Debug, thiserror::Error)]
pub enum AgentProcessCompileErr {
    #[error(transparent)]
    Directory(#[from] crate::agents::LaunchDirRefusal),
    #[error(transparent)]
    Workspace(#[from] crate::workspace::WorkspaceErr),
    #[error(transparent)]
    Paths(#[from] crate::disk::paths::PathErr),
    #[error(transparent)]
    AccountBudget(#[from] crate::config::AccountBudgetConfigError),
    #[error("agent `{kind}` cannot launch: {conflict}")]
    SettingsConflict {
        kind: String,
        conflict: crate::agents::SettingsConflict,
    },
    #[error("{kind} does not accept {flag} under RimZ: {reason}")]
    RejectedArg {
        kind: String,
        flag: &'static str,
        reason: &'static str,
    },
    #[error(transparent)]
    LaunchSettings(#[from] crate::agents::skills::LaunchSettingsErr),
    #[error(transparent)]
    Login(#[from] crate::agents::RoomLoginErr),
    #[error("unknown agent kind `{kind}`")]
    UnknownAgent { kind: String },
    #[error("agent `{kind}` has no launch command")]
    NoLaunch { kind: String },
    #[error("agent `{kind}` has no resume command")]
    NoResume { kind: String },
    #[error("agent `{kind}` has no session picker")]
    NoPicker { kind: String },
    #[error("agent `{kind}` has no fork command")]
    NoFork { kind: String },
    #[error("agent `{kind}` produced an empty launch command")]
    EmptyCommand { kind: String },
    #[error(
        "`{kind}` prompt is {size} bytes, exceeding RimZ's {limit}-byte argv safety limit; shorten it or move detail into a file the agent reads"
    )]
    PromptTooLarge {
        kind: String,
        size: usize,
        limit: usize,
    },
    #[error(
        "`{kind}` CLI reads a prompt beginning with `-` as an option and would drop it; start the prompt with a word, or send it with `rimz message` once the agent is up"
    )]
    PositionalPromptOption { kind: String },
    #[error(transparent)]
    Trust(#[from] crate::trust::TrustErr),
    #[error(transparent)]
    Shell(#[from] AgentShellErr),
    #[error(
        "agent `{kind}` env is configured in {}/.rimz/config.toml but the project is {state}; {fix}",
        root.display()
    )]
    BlockedEnv {
        kind: String,
        root: PathBuf,
        state: &'static str,
        fix: &'static str,
    },
    #[error(
        "agent `{kind}` launch env key `{key}` is invalid; environment variable names must be non-empty, cannot contain `=`, and cannot start with `-`"
    )]
    InvalidEnvKey { kind: String, key: String },
}

type AgentProcessResult<T> = std::result::Result<T, AgentProcessCompileErr>;

#[derive(Clone, PartialEq, Eq)]
enum ProcessWrapping {
    LoginShell,
    Direct,
}

#[derive(Clone, PartialEq, Eq)]
pub struct CompiledAgentProcess {
    pub host_skills: Option<crate::agents::skills::HostSkillPlan>,
    pub(super) env_warnings: Vec<String>,
    /// What the adapter's MCP registration could not apply, one line each.
    pub mcp_warnings: Vec<String>,
    pub(super) settings_artifact: Option<crate::agents::skills::LaunchSettingsArtifact>,
    /// Provider command before shell startup wrapping.
    pub provider_argv: Vec<String>,
    /// Provider executable, as the launch names it before the step resolves it.
    pub provider_program: String,
    /// Final shell-wrapped command.
    pub argv: Vec<String>,
    /// Final child environment, also re-applied after shell startup.
    pub env: BTreeMap<String, String>,
    pub secret_keys: BTreeSet<String>,
    pub reminder: String,
    /// Environment keys removed before execution and again after shell startup.
    pub unset: BTreeSet<String>,
    /// The one shell every wrapper of this launch runs under.
    shell: Option<PathBuf>,
    wrapping: ProcessWrapping,
    session_step_argv: Vec<String>,
}

impl CompiledAgentProcess {
    /// The provider settings file this launch writes, as JSON.
    pub fn settings(&self) -> Option<&serde_json::Value> {
        self.settings_artifact
            .as_ref()
            .map(|(_, settings, _)| settings)
    }

    pub(super) fn rebuild_argv(&mut self) {
        let argv = self
            .session_step_argv
            .iter()
            .chain(&self.provider_argv)
            .cloned()
            .collect::<Vec<_>>();
        self.argv = match self.wrapping {
            ProcessWrapping::LoginShell => self.login_shell_argv(&argv),
            ProcessWrapping::Direct => argv,
        };
    }

    /// Keep mount-planning inputs identical in the final provider environment.
    pub(super) fn pin_env(&mut self, pins: BTreeMap<String, crate::sandbox::EnvPin>) {
        for (key, pin) in pins {
            match pin {
                crate::sandbox::EnvPin::Set(value) => {
                    self.unset.remove(&key);
                    self.env.insert(key, value);
                }
                crate::sandbox::EnvPin::Unset => {
                    self.env.remove(&key);
                    self.unset.insert(key);
                }
            }
        }
        self.rebuild_argv();
    }

    pub(super) fn login_shell_argv(&self, agent_argv: &[String]) -> Vec<String> {
        login_shell_argv_with(
            self.shell.as_deref(),
            Path::new(ENV_BIN).is_file(),
            &self.env,
            &self.unset,
            agent_argv,
        )
    }

    /// Ask this launch's step to add the adapter's changed-system-prompt arguments, which
    /// depend on the provider version only the step can read.
    pub(super) fn flag_changed_system_prompt(&mut self) {
        let Some(separator) = self.session_step_argv.pop() else {
            return;
        };
        self.session_step_argv
            .extend(["--changed-system-prompt".to_owned(), separator]);
        self.rebuild_argv();
    }
}

/// Launch env carries trusted `[[agents]]` env, which is where a project keeps
/// provider credentials, and the login-shell wrapper repeats every entry as a
/// `KEY=VALUE` token inside `argv`. Both renders print keys only, so no debug
/// format of a compiled process puts a secret into a log, a panic, or an error.
pub const REDACTED_ENV_VALUE: &str = "<redacted>";

pub fn redact_env_tokens(argv: &[String], keys: impl Fn(&str) -> bool) -> Vec<String> {
    argv.iter()
        .map(|arg| match arg.split_once('=') {
            Some((key, _)) if keys(key) => format!("{key}={REDACTED_ENV_VALUE}"),
            _ => arg.clone(),
        })
        .collect()
}

struct RedactedEnv<'a>(&'a BTreeMap<String, String>);

impl std::fmt::Debug for RedactedEnv<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.0.keys().map(|key| (key, REDACTED_ENV_VALUE)))
            .finish()
    }
}

struct RedactedArgv<'a> {
    argv: &'a [String],
    env: &'a BTreeMap<String, String>,
}

impl std::fmt::Debug for RedactedArgv<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(redact_env_tokens(self.argv, |key| {
                self.env.contains_key(key)
            }))
            .finish()
    }
}

impl std::fmt::Debug for CompiledAgentProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledAgentProcess")
            .field("provider_argv", &self.provider_argv)
            .field("provider_program", &self.provider_program)
            .field(
                "argv",
                &RedactedArgv {
                    argv: &self.argv,
                    env: &self.env,
                },
            )
            .field("env", &RedactedEnv(&self.env))
            .field("secret_keys", &self.secret_keys)
            .field("unset", &self.unset)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum AgentProcessStage {
    Ready(CompiledAgentProcess),
    LoginShellReentry {
        process: CompiledAgentProcess,
        argv: Vec<String>,
        prompt_artifacts: Vec<PromptArtifact>,
    },
}

impl std::fmt::Debug for AgentProcessStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ready(process) => f.debug_tuple("Ready").field(process).finish(),
            Self::LoginShellReentry { process, argv, .. } => f
                .debug_struct("LoginShellReentry")
                .field("process", process)
                .field(
                    "argv",
                    &RedactedArgv {
                        argv,
                        env: &process.env,
                    },
                )
                .finish(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AgentProcessStageErr {
    #[error(transparent)]
    Compile(#[from] AgentProcessCompileErr),
    #[error(transparent)]
    Wire(#[from] ExecWireErr),
    #[error("provider-account binding applies only to fresh managed Qwen launches")]
    InvalidProviderBinding,
    #[error(
        "Qwen provider account changed after launch preflight; retry the managed run so quota can be checked against the final account"
    )]
    FinalizedProviderMismatch,
    #[error("finalized Qwen launch produced an empty command")]
    EmptyReentry,
}

/// The identity a `rimz agents exec` pane carries in its structured request
/// and as RIMZ_* env for lifecycle hooks and peer
/// attribution.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecIdentity {
    /// An explicit resume selection bypasses legacy observed-model replay.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resume_model_override: bool,
    pub name: Option<String>,
    /// Provenance for `name`: true only for a user-chosen `--name`, false for
    /// minted and soft names. Carried in the hidden request, not an env var.
    #[serde(default)]
    pub name_explicit: bool,
    /// Stable RimZ launch id. Fresh launches also carry `name`; resumed
    /// provider sessions may have only their durable session identity.
    pub launch_id: Option<String>,
    /// The provisional key of the row this launch binds, fails and reads card
    /// evidence by, when it is not `launch_id`: a swap reserves a fresh key
    /// under the agent's launch id. Absent means the launch id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_key: Option<String>,
    /// Canonical launch parameters. `kind_ordinal` remains display-only and is
    /// deliberately absent from wrapper argv and environment wiring.
    #[serde(default)]
    pub params: crate::agents::LaunchParams,
}

enum LaunchFieldValue<'a> {
    Text(&'a mut Option<String>),
    Ordinal(&'a mut Option<u32>),
}

/// One env-backed launch parameter: the key both the pane env encoder and the
/// lifecycle hook decoder use, so the two sides cannot drift apart.
struct LaunchField {
    env: &'static str,
    encode: fn(&crate::agents::LaunchParams) -> Option<String>,
    target: fn(&mut crate::agents::LaunchParams) -> LaunchFieldValue<'_>,
}

const LAUNCH_FIELDS: [LaunchField; 9] = [
    LaunchField {
        env: crate::harness::launch::ENV_AGENT_ROLE,
        encode: |params| params.role.clone(),
        target: |params| LaunchFieldValue::Text(&mut params.role),
    },
    LaunchField {
        env: crate::harness::launch::ENV_TEAM,
        encode: |params| params.team.clone(),
        target: |params| LaunchFieldValue::Text(&mut params.team),
    },
    LaunchField {
        env: crate::harness::launch::ENV_LAUNCH_GROUP,
        encode: |params| params.launch_group.clone(),
        target: |params| LaunchFieldValue::Text(&mut params.launch_group),
    },
    LaunchField {
        env: crate::harness::launch::ENV_LAUNCH_ORDINAL,
        encode: |params| params.launch_ordinal.map(|value| value.to_string()),
        target: |params| LaunchFieldValue::Ordinal(&mut params.launch_ordinal),
    },
    LaunchField {
        env: crate::harness::launch::ENV_AGENT_PROFILE,
        encode: |params| params.profile.clone(),
        target: |params| LaunchFieldValue::Text(&mut params.profile),
    },
    LaunchField {
        env: crate::harness::launch::ENV_AGENT_MODEL,
        encode: |params| params.model.clone(),
        target: |params| LaunchFieldValue::Text(&mut params.model),
    },
    LaunchField {
        env: crate::harness::launch::ENV_AGENT_EFFORT,
        encode: |params| params.effort.clone(),
        target: |params| LaunchFieldValue::Text(&mut params.effort),
    },
    LaunchField {
        env: crate::harness::launch::ENV_AGENT_BUDGET,
        encode: |params| params.budget.clone(),
        target: |params| LaunchFieldValue::Text(&mut params.budget),
    },
    LaunchField {
        env: crate::harness::launch::ENV_AGENT_EPOCH,
        encode: |params| Some(params.epoch.unwrap_or(0).to_string()),
        target: |params| LaunchFieldValue::Ordinal(&mut params.epoch),
    },
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ExecAction {
    Launch {
        prompt: Option<String>,
        #[serde(default)]
        extra_args: Vec<String>,
    },
    Pick {
        #[serde(default)]
        extra_args: Vec<String>,
    },
    Resume {
        session_id: String,
        #[serde(default)]
        extra_args: Vec<String>,
    },
    Fork {
        session_id: String,
        #[serde(default)]
        extra_args: Vec<String>,
    },
}

impl ExecAction {
    pub(super) fn extra_args(&self) -> &[String] {
        match self {
            Self::Launch { extra_args, .. }
            | Self::Pick { extra_args }
            | Self::Resume { extra_args, .. }
            | Self::Fork { extra_args, .. } => extra_args,
        }
    }

    pub fn extra_args_mut(&mut self) -> &mut Vec<String> {
        match self {
            Self::Launch { extra_args, .. }
            | Self::Pick { extra_args }
            | Self::Resume { extra_args, .. }
            | Self::Fork { extra_args, .. } => extra_args,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProviderAccountState {
    #[default]
    Unbound,
    Pending {
        binding: crate::agents::ProviderAccountBinding,
    },
    Finalized {
        binding: crate::agents::ProviderAccountBinding,
    },
}

impl<'de> Deserialize<'de> for ProviderAccountState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "state", rename_all = "snake_case")]
        enum RawState {
            Unbound,
            Pending {
                binding: crate::agents::ProviderAccountBinding,
            },
            Finalized {
                binding: Option<crate::agents::ProviderAccountBinding>,
            },
        }

        match RawState::deserialize(deserializer)? {
            RawState::Unbound => Ok(Self::Unbound),
            RawState::Pending { binding } => Ok(Self::Pending { binding }),
            RawState::Finalized {
                binding: Some(binding),
            } => Ok(Self::Finalized { binding }),
            RawState::Finalized { binding: None } => Err(serde::de::Error::custom(
                "finalized provider-account launch is missing its expected binding",
            )),
        }
    }
}

impl ProviderAccountState {
    fn binding(&self) -> Option<&crate::agents::ProviderAccountBinding> {
        match self {
            Self::Unbound => None,
            Self::Pending { binding } | Self::Finalized { binding } => Some(binding),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReminderSection {
    Loop(String),
    Workflow(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation_default: Option<crate::config::Isolation>,
    pub kind: AgentKind,
    pub action: ExecAction,
    #[serde(default)]
    pub system_prompt_file: Option<crate::config::PromptSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub append_system_prompt_files: Vec<crate::config::PromptSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_prompt: Option<crate::harness::team_prompt::TeamPrompt>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::config::deserialize_optional_skill_list"
    )]
    pub skills: Option<Vec<crate::config::SkillName>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcps: Option<Vec<crate::config::McpName>>,
    /// Whether the applied tools list excludes `Skill`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_skill_tool: bool,
    /// The RimZ capabilities a `capabilities:` list grants, each with its load mark; absent for
    /// a profile that reaches them through `skills` and `mcps`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rimz_capabilities: Option<Vec<crate::config::RimzEntry>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<crate::config::ToolRule>>,
    /// The definition's tools list as canonical base names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    #[serde(default)]
    pub provider_account: ProviderAccountState,
    pub run_id: Option<RunId>,
    pub worktree_path: Option<PathBuf>,
    #[serde(default)]
    pub close_pane_on_exit: bool,
    #[serde(default)]
    pub exit_on_run_completion: bool,
    /// Whether this process is a supervised child launched through
    /// `rimz subagents`.
    #[serde(default)]
    pub subagent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up_message: Option<MessageId>,
    /// Whether the wrapper queues one continue to the resumed session after
    /// spawn, unless a stage re-wake stands in for it. Set by
    /// `rimz agents restart` for an interrupted agent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub continue_session: bool,
    /// The reminder section composed or replayed for this launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reminder_section: Option<ReminderSection>,
    #[serde(default)]
    pub identity: ExecIdentity,
}

impl ExecRequest {
    /// The skills callable with the launch's applied tools and profile policy,
    /// less the rimz skills `hidden` because the launch registers the MCP
    /// tools of the same name.
    ///
    /// On a request with a RimZ grant, the grant names the skills of RimZ's MCP capabilities and
    /// a `skills` list every other skill, so the list's capability names grant nothing and the
    /// granted names join it.
    pub fn callable_skills(&self, hidden: &[&str]) -> Option<Vec<crate::config::SkillName>> {
        if self.no_skill_tool {
            return Some(Vec::new());
        }
        let grant = self.rimz_capabilities.as_deref();
        self.skills.as_ref().map(|skills| {
            skills
                .iter()
                .filter(|skill| grant.is_none() || !is_capability_name(skill.as_str()))
                .cloned()
                .chain(
                    grant
                        .into_iter()
                        .flatten()
                        .filter(|entry| entry.capability().is_some())
                        .filter_map(|entry| entry.name().parse().ok()),
                )
                .filter(|skill| !hidden.contains(&skill.as_str()))
                .collect()
        })
    }

    /// Whether skill `name` is callable on this launch: under a RimZ grant a capability's skill
    /// is callable only when the grant names it, and any other skill as the `skills` list says.
    pub(super) fn skill_callable(&self, hidden: &[&str], name: &str) -> bool {
        if self.rimz_capabilities.is_some() && is_capability_name(name) {
            return !self.no_skill_tool && self.granted(name);
        }
        crate::config::skill_is_callable(self.callable_skills(hidden).as_deref(), name)
    }

    /// Whether the request's RimZ grant names `name`.
    fn granted(&self, name: &str) -> bool {
        self.rimz_capabilities
            .iter()
            .flatten()
            .any(|entry| entry.name() == name)
    }

    /// The request's RimZ grant, which each skill enforcer applies to the capability skills it
    /// enumerates: a restriction the profile set, which a launch applies or refuses, never skips.
    pub(super) fn skill_grant(&self) -> Option<&[crate::config::RimzEntry]> {
        self.rimz_capabilities.as_deref()
    }

    /// Whether this launch is a workflow judge, whose checkout is read-only.
    pub(super) fn is_judge(&self) -> bool {
        self.identity
            .params
            .workflow
            .as_ref()
            .is_some_and(|workflow| workflow.role == crate::agents::WorkflowAttemptRole::Judge)
    }

    /// Whether the launch may not delegate: a child, whose every launch would be refused,
    /// and a judge, whose child would get a writable checkout.
    pub fn cannot_delegate(&self) -> bool {
        self.subagent || self.is_judge()
    }

    /// The RimZ capabilities the launch's grant, or else its `mcps` selection, serves, less
    /// those `denied` it (`LaunchReminders::denied_capabilities`), and less `rimz-loop` on a
    /// launch that cannot delegate: a booking's fire resolves no caller, so it would launch a
    /// top-level agent past the child's refusal.
    pub(super) fn mcp_capabilities(
        &self,
        denied: &[crate::mcp::catalog::Capability],
    ) -> Vec<crate::mcp::catalog::Capability> {
        use crate::mcp::catalog::Capability;
        let mut capabilities = match &self.rimz_capabilities {
            Some(_) => self
                .served_marks()
                .into_iter()
                .map(|(capability, _)| capability)
                .collect(),
            None => crate::config::mcp_capabilities(self.mcps.as_deref()),
        };
        capabilities.retain(|capability| !denied.contains(capability));
        if self.cannot_delegate() {
            capabilities.retain(|capability| *capability != Capability::Loop);
        }
        capabilities
    }

    /// Each capability the grant, or else the `mcps` selection, serves, with its load mark: its
    /// own entry's, else the `rimz` entry's.
    pub(super) fn served_marks(
        &self,
    ) -> Vec<(
        crate::mcp::catalog::Capability,
        Option<crate::mcp::catalog::LoadMode>,
    )> {
        let Some(grant) = &self.rimz_capabilities else {
            return crate::config::served_marks(self.mcps.as_deref());
        };
        let server_mark = grant
            .iter()
            .find(|entry| entry.is_server())
            .and_then(crate::config::RimzEntry::mode);
        crate::mcp::catalog::Capability::ALL
            .into_iter()
            .filter_map(|capability| {
                let entry = grant
                    .iter()
                    .find(|entry| entry.capability() == Some(capability))?;
                Some((capability, entry.mode().or(server_mark)))
            })
            .collect()
    }

    /// The `mcps` list a provider's MCP code reads. With no grant it is the profile's own.
    /// Under a grant it is the user's entries of `mcps` (`*` and their own servers), or `*`
    /// when `mcps` is absent, then each granted capability and the `rimz` entry, with their marks.
    pub(super) fn mcp_list(&self) -> Option<std::borrow::Cow<'_, [crate::config::McpName]>> {
        let Some(grant) = &self.rimz_capabilities else {
            return self.mcps.as_deref().map(std::borrow::Cow::Borrowed);
        };
        let own = self.mcps.as_ref().map_or_else(
            || vec![crate::config::McpName::all()],
            |entries| {
                entries
                    .iter()
                    .filter(|entry| entry.is_all() || entry.user_server().is_some())
                    .cloned()
                    .collect()
            },
        );
        let granted = grant
            .iter()
            .filter(|entry| entry.capability().is_some() || entry.is_server())
            .filter_map(|entry| entry.to_string().parse().ok());
        Some(std::borrow::Cow::Owned(
            own.into_iter().chain(granted).collect(),
        ))
    }

    /// The profile list handed to the provider, less the skills `withheld` from the model
    /// (`LaunchReminders::withheld_skills`). Under a RimZ grant the list holds no capability's
    /// name: each enforcer admits the granted capability skills it finds through
    /// `ExecRequest::skill_grant`.
    pub(super) fn launch_skills(&self, withheld: &[&str]) -> Option<Vec<crate::config::SkillName>> {
        self.skills.as_ref().map(|skills| {
            skills
                .iter()
                .filter(|skill| {
                    self.rimz_capabilities.is_none() || !is_capability_name(skill.as_str())
                })
                .filter(|skill| !withheld.contains(&skill.as_str()))
                .cloned()
                .collect()
        })
    }

    /// The skills a provider's allow rules name: `listed` and each granted capability that is
    /// not `withheld`.
    fn allowed_skills(
        &self,
        listed: &[crate::config::SkillName],
        withheld: &[&str],
    ) -> Vec<crate::config::SkillName> {
        listed
            .iter()
            .cloned()
            .chain(
                self.rimz_capabilities
                    .iter()
                    .flatten()
                    .filter(|entry| entry.capability().is_some())
                    .filter(|entry| !withheld.contains(&entry.name()))
                    .filter_map(|entry| entry.name().parse().ok()),
            )
            .collect()
    }

    pub fn fresh(
        cell: &AgentCell,
        identity: ExecIdentity,
        worktree_path: Option<PathBuf>,
        close_pane_on_exit: bool,
    ) -> Self {
        Self {
            system_prompt_file: cell.system_prompt_file.clone(),
            append_system_prompt_files: cell.append_system_prompt_files.clone(),
            team_prompt: cell.team_prompt.clone(),
            skills: cell.skills.clone(),
            mcps: cell.mcps.clone(),
            no_skill_tool: cell.no_skill_tool,
            rimz_capabilities: cell.rimz_capabilities.clone(),
            allowed_tools: cell.allowed_tools.clone(),
            tools: cell.tools.clone(),
            isolation_default: cell.isolation_default,
            identity,
            worktree_path,
            close_pane_on_exit,
            ..Self::bare_launch(cell.kind.clone(), cell.args.clone())
        }
    }

    pub fn bare_launch(kind: AgentKind, extra_args: Vec<String>) -> Self {
        Self {
            kind,
            action: ExecAction::Launch {
                prompt: None,
                extra_args,
            },
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            team_prompt: None,
            skills: None,
            mcps: None,
            no_skill_tool: false,
            rimz_capabilities: None,
            allowed_tools: None,
            tools: None,
            isolation_default: None,
            provider_account: ProviderAccountState::Unbound,
            run_id: None,
            worktree_path: None,
            close_pane_on_exit: false,
            exit_on_run_completion: false,
            subagent: false,
            follow_up_message: None,
            continue_session: false,
            reminder_section: None,
            identity: ExecIdentity::default(),
        }
    }
}

/// The name of a capability of RimZ's MCP server, whose skill under a RimZ grant only the grant
/// can make callable.
fn is_capability_name(name: &str) -> bool {
    crate::mcp::catalog::Capability::from_name(name).is_some()
}

#[derive(Serialize, Deserialize)]
struct ExecWire {
    #[serde(flatten)]
    request: ExecRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt_file: Option<PathBuf>,
}

/// A validated exec envelope whose launch prompt may still live in an artifact.
pub struct ExecEnvelope(ExecWire);

impl ExecEnvelope {
    /// Inspect identity and run context for failure cleanup only. A launch prompt
    /// is not yet materialized; this borrowed request must not be compiled.
    pub fn request(&self) -> &ExecRequest {
        &self.0.request
    }

    /// Restore the prompt before the request can enter provider compilation.
    pub fn materialize(self) -> Result<ExecRequest, ExecWireErr> {
        let ExecWire {
            mut request,
            prompt_file,
        } = self.0;
        if let (Some(path), ExecAction::Launch { prompt, .. }) = (prompt_file, &mut request.action)
        {
            *prompt = Some(
                std::fs::read_to_string(&path)
                    .map_err(|source| ExecWireErr::PromptRead { path, source })?,
            );
        }
        Ok(request)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExecWireErr {
    #[error(transparent)]
    Skills(#[from] crate::config::SkillListErr),
    #[error("serializing hidden agent exec request: {0}")]
    Serialize(#[source] serde_json::Error),
    #[error("parsing hidden agent exec request: {0}")]
    Parse(#[source] serde_json::Error),
    #[error("writing launch prompt artifact: {0}")]
    PromptWrite(#[source] crate::disk::atomic::AtomicErr),
    #[error("reading launch prompt `{}`: {source}", path.display())]
    PromptRead {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("hidden agent exec kind `{payload}` does not match visible kind `{visible}`")]
    KindMismatch { visible: String, payload: String },
    #[error("hidden agent exec worktree does not match visible --worktree-path")]
    WorktreeMismatch,
    #[error("--launch-id requires --agent-name")]
    OrphanLaunchId,
    #[error("--exit-on-run-completion requires --run-id")]
    MissingRunId,
    #[error("hidden provider-account launch binding is empty")]
    EmptyProviderBinding,
    #[error("kind_ordinal is display-only and cannot be carried by the agent exec request")]
    KindOrdinal,
}

/// Compile the selected adapter action and preserve provider trailing argv.
pub fn compile_provider_argv(
    adapter: &crate::agents::AgentDefinition,
    kind: &str,
    action: &ExecAction,
    cwd: &Path,
) -> AgentProcessResult<Vec<String>> {
    if let Some(rejected) = adapter.rejected_extra_arg(action.extra_args()) {
        return Err(AgentProcessCompileErr::RejectedArg {
            kind: kind.to_owned(),
            flag: rejected.flag,
            reason: rejected.reason,
        });
    }
    if let ExecAction::Launch {
        prompt: Some(prompt),
        ..
    } = action
        && prompt.len() > TEXT_PROMPT_LIMIT
    {
        return Err(AgentProcessCompileErr::PromptTooLarge {
            kind: kind.to_owned(),
            size: prompt.len(),
            limit: TEXT_PROMPT_LIMIT,
        });
    }
    if let ExecAction::Launch {
        prompt: Some(prompt),
        ..
    } = action
        && matches!(
            adapter.spec().launch.prompt,
            crate::agents::definition::PromptStyle::Positional
        )
        && prompt.starts_with('-')
    {
        return Err(AgentProcessCompileErr::PositionalPromptOption {
            kind: kind.to_owned(),
        });
    }
    let argv =
        match action {
            ExecAction::Launch { prompt, extra_args } => adapter
                .launch_command(extra_args, prompt.as_deref())
                .ok_or_else(|| AgentProcessCompileErr::NoLaunch {
                    kind: kind.to_owned(),
                })?,
            ExecAction::Pick { extra_args } => {
                let picker = adapter.spec().launch.picker.ok_or_else(|| {
                    AgentProcessCompileErr::NoPicker {
                        kind: kind.to_owned(),
                    }
                })?;
                picker
                    .iter()
                    .map(|arg| (*arg).to_owned())
                    .chain(extra_args.iter().cloned())
                    .collect()
            }
            ExecAction::Resume {
                session_id,
                extra_args,
            } => {
                let mut argv = adapter.resume_command(session_id, cwd).ok_or_else(|| {
                    AgentProcessCompileErr::NoResume {
                        kind: kind.to_owned(),
                    }
                })?;
                argv.extend(extra_args.iter().cloned());
                argv
            }
            ExecAction::Fork {
                session_id,
                extra_args,
            } => {
                let mut argv = adapter
                    .spec()
                    .launch
                    .fork_command(session_id)
                    .ok_or_else(|| AgentProcessCompileErr::NoFork {
                        kind: kind.to_owned(),
                    })?;
                argv.extend(extra_args.iter().cloned());
                argv
            }
        };
    if argv.is_empty() {
        return Err(AgentProcessCompileErr::EmptyCommand {
            kind: kind.to_owned(),
        });
    }
    Ok(argv)
}

/// Compile provider argv, launch environment, and login-shell wrapper together.
fn compile_agent_process(
    project_root: &Path,
    request: &ExecRequest,
    cwd: &Path,
    host_runtime: Option<&RuntimePaths>,
) -> AgentProcessResult<CompiledAgentProcess> {
    let machine = crate::config::MachineConfig::load_lenient();
    let login = crate::agents::session_login(
        &request.kind,
        request.identity.params.login.as_ref(),
        &machine.accounts,
    )?;
    compile_agent_process_with_extra_env(
        host_runtime,
        project_root,
        request,
        cwd,
        &login.env(&BTreeMap::new()),
        &LaunchReminders {
            agent_shell: machine.agents.shell.clone(),
            ..LaunchReminders::default()
        },
    )
}

fn compile_agent_process_with_extra_env(
    host_runtime: Option<&RuntimePaths>,
    project_root: &Path,
    request: &ExecRequest,
    cwd: &Path,
    extra_env: &BTreeMap<String, String>,
    reminders: &LaunchReminders,
) -> AgentProcessResult<CompiledAgentProcess> {
    let kind = request.kind.as_str();
    let machine = crate::config::MachineConfig::load_lenient();
    machine
        .accounts
        .validate_budget_for_login(&crate::ids::LoginKey::new(
            request.kind.clone(),
            request.identity.params.login.clone().unwrap_or_default(),
        ))?;
    let adapter = crate::agents::find_definition(kind).ok_or_else(|| {
        AgentProcessCompileErr::UnknownAgent {
            kind: kind.to_owned(),
        }
    })?;
    let user_shell = crate::proc::user_shell();
    let (shell, shell_env) = match reminders.agent_shell.as_deref() {
        Some(configured) => {
            let state = StatePaths::for_project_root(project_root)?;
            let shell = resolve_agent_shell(configured, Some(&state.workspace_record)).path?;
            let env = agent_shell_env(&shell, user_shell.as_deref());
            (Some(shell), env)
        }
        None => (user_shell, BTreeMap::new()),
    };
    let trusted_env = trusted_agent_env(project_root, kind)?;
    let secret_keys: BTreeSet<_> = trusted_env.keys().cloned().collect();
    let mut launch_env = extra_env.clone();
    launch_env.extend(shell_env);
    let mut env = compose_agent_env(trusted_env, adapter, request, &launch_env)?;
    let mut login_env = match &reminders.settings {
        Some((_, env)) => env.clone(),
        None => crate::agents::session_login(
            &request.kind,
            request.identity.params.login.as_ref(),
            &machine.accounts,
        )?
        .env(&crate::agents::ambient_env()),
    };
    login_env.extend(env.clone());
    for dir in &request.identity.params.add_dirs {
        let repo_root = crate::workspace::main_repo_root(Path::new(dir))?;
        crate::agents::preflight_launch_dir(
            adapter,
            Path::new(dir),
            repo_root.as_deref(),
            &login_env,
            crate::agents::LaunchDirMode::Added,
        )?;
    }
    let code_mode = request.identity.params.code_mode;
    let release_code_mode = match adapter.spec().launch.presets.code_mode {
        crate::agents::definition::CodeModeSwitch::Config { marker } => {
            code_mode.is_none()
                && login_env.get(ENV_CODE_MODE_CONFIG).map(String::as_str) == Some(marker)
        }
        _ => false,
    };
    let env_warnings = adapter.finalize_launch_env(
        &login_env,
        &mut env,
        reminders.disable_native_subagents(request),
        reminders.system_prompt_file.as_deref(),
        code_mode,
        release_code_mode,
    );
    let ask = adapter
        .spec()
        .launch
        .permission_args(crate::agents::PermissionMode::Ask);
    let hooks_disabled = adapter
        .hooks_installed(&login_env)
        .then(|| adapter.hooks_disabled(cwd, &login_env))
        .flatten();
    let conflict = hooks_disabled.or_else(|| {
        if ask.is_empty()
            || !request
                .action
                .extra_args()
                .windows(ask.len())
                .any(|args| args == ask.as_slice())
        {
            return None;
        }
        adapter.ask_unenforced(cwd, &login_env)
    });
    if let Some(conflict) = conflict {
        return Err(AgentProcessCompileErr::SettingsConflict {
            kind: kind.to_owned(),
            conflict,
        });
    }
    let mut action = request.action.clone();
    if reminders.lsp_configured && reminders.lsp_callable(request) {
        adapter.disable_native_lsp_args(action.extra_args_mut());
    }
    if reminders.disable_native_subagents(request) {
        adapter.disable_native_subagent_args(action.extra_args_mut());
    }
    if reminders.sandbox {
        adapter.disable_native_sandbox_args(action.extra_args_mut());
    }
    let isolation = crate::config::Isolation::ambient(extra_env).unwrap_or_default();
    if let Some(roots) = &reminders.permission_roots {
        let linked = crate::worktree::linked_worktree(cwd);
        let roots = roots
            .iter()
            .cloned()
            .chain(request.identity.params.add_dirs.iter().map(PathBuf::from))
            .collect::<Vec<_>>();
        adapter.configure_room_permissions(
            action.extra_args_mut(),
            &login_env,
            (
                cwd,
                linked.as_ref().map(|worktree| worktree.primary.as_path()),
            ),
            &roots,
            isolation,
        );
    }
    // A settings-member fast switch rides outside the settings document until
    // every other settings mutation has run, then merges into the one result.
    let fast_settings = adapter.take_fast_settings(action.extra_args_mut());
    let reminder =
        crate::harness::launch_reminders::render(request, reminders, cwd, shell.as_deref());
    let channel = adapter.append_system_text_channel();
    if let Some(channel) = &channel {
        merge_appended_system_text(
            action.extra_args_mut(),
            channel,
            &reminder,
            &adapter.system_prompt_arg_matchers(),
            cwd,
        )?;
    }
    let skill_env = login_env;
    let skill_chain = crate::agents::skills::SkillChain::from_env(&skill_env);
    let withheld = reminders.withheld_skills(request);
    let listed = request.launch_skills(&withheld);
    let grant = request.skill_grant();
    let (host_skills, mut settings_artifact) = if let Some(runtime) = host_runtime
        && (listed.is_some()
            || grant.is_some()
            || (!withheld.is_empty()
                && matches!(
                    adapter.spec().host_skills,
                    crate::agents::skills::HostSkills::Switch { .. }
                ))) {
        let artifact_dir = runtime.prompt_dir();
        let mut args = action.extra_args().to_vec();
        match adapter.spec().host_skills.apply(
            (listed.as_deref(), &withheld, grant),
            adapter.skills_home(&skill_env).as_deref(),
            skill_chain.as_ref(),
            (cwd, &artifact_dir),
            &mut args,
        ) {
            Ok((plan, artifact)) => {
                *action.extra_args_mut() = args;
                (Some(plan), artifact)
            }
            Err(error) if listed.is_none() && grant.is_none() => {
                tracing::debug!(
                    %error,
                    "keeping native skill discovery because the optional host view is unavailable"
                );
                (None, None)
            }
            Err(error) => return Err(error.into()),
        }
    } else {
        (None, None)
    };
    adapter.fold_permission_settings(
        (
            cwd,
            reminders
                .settings
                .as_ref()
                .map_or(cwd, |(dir, _)| dir.as_path()),
        ),
        action.extra_args_mut(),
        &mut settings_artifact,
    )?;
    if let (Some(listed), Some((artifact_dir, _))) = (&listed, &reminders.settings) {
        adapter.allow_listed_skill_args(
            &request.allowed_skills(listed, &withheld),
            (cwd, artifact_dir),
            action.extra_args_mut(),
            &mut settings_artifact,
        )?;
    }
    if let (Some(rules), Some((artifact_dir, _))) = (&request.allowed_tools, &reminders.settings)
        && let crate::agents::skills::ToolRules::Settings { render, translate } =
            adapter.spec().tool_rules
    {
        // A list the provider reads replaces the account's saved allows, the install's rule for
        // the tools of a provider-started server among them, so it carries that rule while
        // routine is on. A list it reads none of emits nothing and leaves the saved allows.
        let served_rule = (reminders.routine_rimz.is_some()
            && reminders.mcp_launch_env(request).is_some()
            && rules.iter().any(|rule| translate(rule).is_some()))
        .then(|| format!("mcp__{}", crate::mcp::catalog::SERVER_NAME))
        .and_then(|rule| rule.parse::<crate::config::ToolRule>().ok());
        let rules: Vec<_> = rules.iter().cloned().chain(served_rule).collect();
        render(
            &rules,
            (cwd, artifact_dir),
            action.extra_args_mut(),
            &mut settings_artifact,
        )?;
    }
    if let Some((artifact_dir, dirs)) = &reminders.routine_rimz {
        adapter.allow_routine_rimz_args(
            (cwd, artifact_dir),
            (
                adapter.skills_home(&skill_env).as_deref(),
                skill_chain.as_ref(),
            ),
            dirs,
            action.extra_args_mut(),
            &mut settings_artifact,
        )?;
    }
    let artifact_dir = reminders
        .settings
        .as_ref()
        .map_or(cwd, |(dir, _)| dir.as_path());
    adapter.bridge_folder_trust_args(
        &skill_env,
        (cwd, artifact_dir),
        action.extra_args_mut(),
        &mut settings_artifact,
    )?;
    for run in reminders.notes_runs.iter().chain(&reminders.notes_store) {
        adapter.allow_team_notes_args(
            run,
            (cwd, artifact_dir),
            action.extra_args_mut(),
            &mut settings_artifact,
        )?;
    }
    if !request.identity.params.add_dirs.is_empty() {
        adapter.allow_added_dirs_args(
            &request.identity.params.add_dirs,
            (cwd, artifact_dir),
            action.extra_args_mut(),
            &mut settings_artifact,
        )?;
    }
    if let Some(roots) = &reminders.permission_roots {
        adapter.configure_sandbox_settings(
            (cwd, artifact_dir),
            roots,
            isolation,
            action.extra_args_mut(),
            &mut settings_artifact,
        )?;
    }
    if reminders.disable_native_idle_compaction {
        adapter.disable_native_idle_compaction_args(
            (cwd, artifact_dir),
            action.extra_args_mut(),
            &mut settings_artifact,
        )?;
    }
    if reminders.enable_native_auto_continue {
        adapter.enable_native_auto_continue_args(
            (cwd, artifact_dir),
            action.extra_args_mut(),
            &mut settings_artifact,
        )?;
    }
    if let Some(on) = fast_settings {
        adapter.merge_fast_settings(
            (cwd, artifact_dir),
            on,
            action.extra_args_mut(),
            &mut settings_artifact,
        )?;
    }
    // After the passthrough joined the args, so the exclusion lands in the list the
    // provider honours.
    if request.identity.params.code_mode == Some(false) {
        adapter
            .spec()
            .launch
            .presets
            .code_mode
            .exclude_off(action.extra_args_mut());
    }
    let frontend = adapter.take_frontend(action.extra_args_mut());
    if channel == Some(SystemTextChannel::ExtensionEnv) {
        env.insert(ENV_LAUNCH_REMINDERS.to_owned(), reminder.clone());
    }
    // Stamped or cleared on every launch: an inherited switch would promise
    // a nested launch context its own adapter or config does not deliver.
    let mut unset = BTreeSet::new();
    if reminders.runtime_env {
        env.insert(ENV_RUNTIME_ENV.to_owned(), "1".to_owned());
    } else {
        env.remove(ENV_RUNTIME_ENV);
        unset.insert(ENV_RUNTIME_ENV.to_owned());
    }
    if request.identity.params.fast == Some(true)
        && adapter.spec().launch.presets.fast == crate::agents::definition::FastSwitch::ExtensionEnv
    {
        env.insert(ENV_LAUNCH_FAST.to_owned(), "1".to_owned());
    } else {
        env.remove(ENV_LAUNCH_FAST);
        unset.insert(ENV_LAUNCH_FAST.to_owned());
    }
    release_stamped_pair(
        &mut env,
        &mut unset,
        &skill_env,
        &secret_keys,
        ENV_CODE_MODE_ENV,
    );
    // On and forced off both stamp, so a nested unset launch releases either; off sets
    // its value over the user's export and a trusted `[[agents]]` env value alike.
    if let Some(on) = request.identity.params.code_mode {
        let switch = adapter.spec().launch.presets.code_mode;
        if let Some((key, value)) = switch.env(on) {
            stamp_pair(&mut env, &mut unset, ENV_CODE_MODE_ENV, key, value);
        }
        if let crate::agents::definition::CodeModeSwitch::Config { marker } = switch {
            env.insert(ENV_CODE_MODE_CONFIG.to_owned(), marker.to_owned());
            unset.remove(ENV_CODE_MODE_CONFIG);
        }
    }
    if release_code_mode {
        env.remove(ENV_CODE_MODE_CONFIG);
        unset.insert(ENV_CODE_MODE_CONFIG.to_owned());
    }
    if frontend.extension_auto_approve {
        env.insert(ENV_LAUNCH_AUTO_APPROVE.to_owned(), "1".to_owned());
    } else {
        env.remove(ENV_LAUNCH_AUTO_APPROVE);
        unset.insert(ENV_LAUNCH_AUTO_APPROVE.to_owned());
    }
    let inherited_cache = release_stamped_pair(
        &mut env,
        &mut unset,
        &skill_env,
        &secret_keys,
        ENV_LONG_CACHE_ENV,
    );
    if reminders.long_cache
        && let Some((key, value)) = adapter.spec().launch.long_cache_env
        && (!skill_env.contains_key(key)
            || inherited_cache.is_some_and(|(owned_key, _)| owned_key == key))
    {
        stamp_pair(&mut env, &mut unset, ENV_LONG_CACHE_ENV, key, value);
    }
    // A launch carries its own model, team, and role, never its caller's.
    let params = &request.identity.params;
    for (key, absent) in [
        (ENV_AGENT_MODEL, params.model.is_none()),
        (ENV_TEAM, params.team.is_none()),
        (ENV_AGENT_ROLE, params.role.is_none()),
    ] {
        if absent {
            env.remove(key);
            unset.insert(key.to_owned());
        }
    }
    // A launch hands its own MCP selection to a provider-started server, never its caller's.
    match reminders.mcp_launch_env(request) {
        Some(vars) => {
            for (key, value) in vars {
                unset.remove(key);
                env.insert(key.to_owned(), value);
            }
        }
        None => {
            for key in [
                crate::mcp::launch_env::ENV_SERVE,
                crate::mcp::launch_env::ENV_CWD,
            ] {
                env.remove(key);
                unset.insert(key.to_owned());
            }
        }
    }
    let mut mcp_warnings = Vec::new();
    if adapter.takes_mcp_servers() {
        mcp_warnings = adapter.mcp_server_args(
            &crate::agents::capabilities::McpLaunch {
                server: mcp_server(reminders, &skill_env, &env),
                mcps: request.mcp_list().as_deref(),
                granted: request.rimz_capabilities.is_some(),
                login_env: &skill_env,
            },
            (cwd, artifact_dir),
            action.extra_args_mut(),
            &mut settings_artifact,
        )?;
    }
    let mut provider_argv = compile_provider_argv(adapter, kind, &action, cwd)?;
    if let Some(subcommand) = frontend.subcommand {
        provider_argv.insert(1, subcommand.to_owned());
    }
    let provider_program =
        provider_argv
            .first()
            .cloned()
            .ok_or_else(|| AgentProcessCompileErr::EmptyCommand {
                kind: kind.to_owned(),
            })?;
    let mut process = CompiledAgentProcess {
        host_skills,
        env_warnings,
        mcp_warnings,
        settings_artifact,
        provider_argv,
        provider_program,
        argv: Vec::new(),
        env,
        secret_keys,
        reminder,
        unset,
        shell,
        wrapping: ProcessWrapping::LoginShell,
        session_step_argv: Vec::new(),
    };
    process.rebuild_argv();
    Ok(process)
}

/// RimZ's MCP server as the launch hands it over, the one value the argv channel and an agent
/// file both render: none when `compile` set no server for the launch.
pub(super) fn mcp_server(
    reminders: &LaunchReminders,
    login_env: &BTreeMap<String, String>,
    env: &BTreeMap<String, String>,
) -> Option<crate::agents::capabilities::McpServer> {
    let (command, args) = reminders.mcp_server.as_ref()?;
    Some(crate::agents::capabilities::McpServer {
        command: command.clone(),
        args: args.clone(),
        grant: reminders.routine_rimz.is_some(),
        env_names: spawn_env_names(login_env, env),
    })
}

/// The variables RimZ sets for the provider: the login environment, the
/// compiled one, and the pins the plan and a scoped view set after compile.
/// The names are taken before the launch is wrapped in the user's login
/// shell, so a variable that shell's startup files export is absent; on
/// Codex, `rimz mcp serve --parent-env` covers those. A name the provider
/// ends up without is harmless to list.
fn spawn_env_names(
    login_env: &BTreeMap<String, String>,
    env: &BTreeMap<String, String>,
) -> BTreeSet<String> {
    login_env
        .keys()
        .chain(env.keys())
        .map(String::as_str)
        .chain([
            "RIMZ_WORKFLOW_RUN",
            "RIMZ_WORKFLOW_DIR",
            "RIMZ_WORKFLOW_DEFINITION",
            "RIMZ_WORKFLOW_NODE",
            "TMPDIR",
            "SCCACHE_CLIENT_SIDE",
            crate::config::Isolation::SCOPED_HOST_PATHS_ENV,
        ])
        .map(str::to_owned)
        .collect()
}

/// The argv shape a channel occupies; the extension channel has none.
fn argv_matcher(channel: &SystemTextChannel) -> Option<crate::agents::PresetArgMatcher> {
    use crate::agents::PresetArgMatcher;

    match channel {
        SystemTextChannel::TextFlag { flags } => Some(PresetArgMatcher::TextFlag(flags.clone())),
        SystemTextChannel::ConfigKey { flags, key } => Some(PresetArgMatcher::ConfigKey {
            flags: flags.clone(),
            key: key.clone(),
        }),
        SystemTextChannel::ExtensionEnv | SystemTextChannel::AgentFile => None,
    }
}

fn merge_appended_system_text(
    extra_args: &mut Vec<String>,
    channel: &SystemTextChannel,
    text: &str,
    prompt_matchers: &[crate::agents::PresetArgMatcher],
    cwd: &Path,
) -> Result<(), crate::agents::skills::LaunchSettingsErr> {
    let Some(matcher) = argv_matcher(channel) else {
        return Ok(());
    };
    let Some(existing) = matcher.occurrences(extra_args).into_iter().last() else {
        if let Some((flag, value)) = render_system_text_channel(channel, text) {
            extra_args.extend([flag, value]);
        }
        return Ok(());
    };

    let file_value = prompt_matchers.iter().any(|prompt_matcher| {
        matches!(prompt_matcher, crate::agents::PresetArgMatcher::Flag(_))
            && prompt_matcher
                .occurrences(extra_args)
                .iter()
                .any(|occurrence| occurrence.argv_range == existing.argv_range)
    });
    if file_value {
        let path = cwd.join(&existing.value);
        let existing_text = std::fs::read_to_string(&path).map_err(|source| {
            crate::agents::skills::LaunchSettingsErr::Settings {
                path,
                reason: source.to_string(),
            }
        })?;
        matcher.remove_occurrences(extra_args);
        if let Some((flag, value)) =
            render_system_text_channel(channel, &format!("{existing_text}\n\n{text}"))
        {
            extra_args.extend([flag, value]);
        }
        return Ok(());
    }

    let config_key = match channel {
        SystemTextChannel::ConfigKey { key, .. } => Some(key),
        SystemTextChannel::TextFlag { .. }
        | SystemTextChannel::ExtensionEnv
        | SystemTextChannel::AgentFile => None,
    };
    let existing_text = if config_key.is_some() {
        parse_toml_string_or_raw(&existing.value)
    } else {
        existing.value.clone()
    };
    let merged = format!("{existing_text}\n\n{text}");
    let (single_token_value, separate_value) = match config_key {
        Some(key) => {
            let quoted = toml::Value::String(merged).to_string();
            (quoted.clone(), format!("{key}={quoted}"))
        }
        None => (merged.clone(), merged),
    };
    if existing.argv_range.len() == 1 {
        let arg = &mut extra_args[existing.argv_range.start];
        arg.truncate(arg.len() - existing.value.len());
        arg.push_str(&single_token_value);
    } else {
        extra_args[existing.argv_range.start + 1] = separate_value;
    }
    Ok(())
}

fn render_system_text_channel(channel: &SystemTextChannel, text: &str) -> Option<(String, String)> {
    match channel {
        SystemTextChannel::TextFlag { flags } => Some((flags.first()?.clone(), text.to_owned())),
        SystemTextChannel::ConfigKey { flags, key } => Some((
            flags.first()?.clone(),
            format!("{key}={}", toml::Value::String(text.to_owned())),
        )),
        SystemTextChannel::ExtensionEnv | SystemTextChannel::AgentFile => None,
    }
}

fn parse_toml_string_or_raw(value: &str) -> String {
    value
        .parse::<toml::Value>()
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| value.to_owned())
}

/// Compile one process and resolve managed-account applicability from its final inputs.
/// Supply runtime paths for host isolation to preflight the provider skill switch.
pub fn compile_managed_agent_process(
    project_root: &Path,
    request: &ExecRequest,
    cwd: &Path,
    requested: &crate::agents::ManagedLaunchState,
    host_runtime: Option<&RuntimePaths>,
) -> AgentProcessResult<(CompiledAgentProcess, crate::agents::ManagedLaunchState)> {
    let process = compile_agent_process(project_root, request, cwd, host_runtime)?;
    let state = if matches!(
        requested,
        crate::agents::ManagedLaunchState::PendingResolution
    ) {
        let adapter = crate::agents::find_definition(request.kind.as_str())
            .expect("process compilation already resolved the adapter");
        adapter.resolve_managed_launch(
            cwd,
            &effective_launch_env(&process.env),
            request.identity.params.model.as_deref(),
            &process.provider_argv,
        )
    } else {
        requested.clone()
    };
    Ok((process, state))
}

/// Compile the serialized wrapper stage for a proven managed provider binding.
/// Pending stages re-enter through the login shell once; finalized stages
/// execute the prepared command without a shell after verifying the binding.
/// `reminders` controls model identity, team context, and subagent policy text.
pub(super) fn compile_agent_process_stage_with_extra_env(
    project_root: &Path,
    request: &ExecRequest,
    cwd: &Path,
    rimz_bin: &Path,
    runtime: &RuntimePaths,
    extra_env: &BTreeMap<String, String>,
    reminders: &LaunchReminders,
) -> Result<AgentProcessStage, AgentProcessStageErr> {
    let bound = !matches!(&request.provider_account, ProviderAccountState::Unbound);
    if bound
        && !matches!(
            &request.action,
            ExecAction::Launch { .. } | ExecAction::Pick { .. }
        )
    {
        return Err(AgentProcessStageErr::InvalidProviderBinding);
    }

    let mut process = compile_agent_process_with_extra_env(
        crate::config::Isolation::ambient(extra_env)
            .is_some_and(|isolation| !isolation.is_scoped())
            .then_some(runtime),
        project_root,
        request,
        cwd,
        extra_env,
        reminders,
    )?;
    process.session_step_argv = session_step_argv(request, rimz_bin);
    process.rebuild_argv();
    let managed_launch = if bound {
        let adapter = crate::agents::find_definition(request.kind.as_str()).ok_or_else(|| {
            AgentProcessCompileErr::UnknownAgent {
                kind: request.kind.to_string(),
            }
        })?;
        let state = adapter.resolve_managed_launch(
            cwd,
            &effective_launch_env(&process.env),
            request.identity.params.model.as_deref(),
            &process.provider_argv,
        );
        if !state.exact_account_applies() {
            return Err(AgentProcessStageErr::InvalidProviderBinding);
        }
        Some(state)
    } else {
        None
    };
    finalize_agent_process_stage(process, request, managed_launch.as_ref(), rimz_bin, runtime)
}

/// The in-pane step between the launch shell and the provider, which resolves the program
/// on the pane's PATH and execs it. A pending provider account has none: its stage re-enters
/// `rimz agents exec`, and the finalized stage that follows carries the step.
fn session_step_argv(request: &ExecRequest, rimz_bin: &Path) -> Vec<String> {
    if matches!(
        request.provider_account,
        ProviderAccountState::Pending { .. }
    ) {
        return Vec::new();
    }
    let mut argv = vec![
        rimz_bin.display().to_string(),
        "agents".into(),
        "session-step".into(),
        request.kind.to_string(),
    ];
    argv.extend(model_step_args(request));
    argv.push("--".into());
    argv
}

/// The model the step prepares, for an adapter with a model session step: a launch's model,
/// or a resume's explicit model override.
fn model_step_args(request: &ExecRequest) -> Vec<String> {
    if !crate::agents::spec_by_kind(request.kind.as_str())
        .is_some_and(|spec| spec.launch.model_session_step)
    {
        return Vec::new();
    }
    let Some(model) = request
        .identity
        .params
        .model
        .as_deref()
        .filter(|model| !model.is_empty())
    else {
        return Vec::new();
    };
    let session = match &request.action {
        ExecAction::Launch { .. } => None,
        ExecAction::Resume { session_id, .. } if request.identity.resume_model_override => {
            Some(session_id)
        }
        _ => return Vec::new(),
    };
    let mut argv = vec!["--model".into(), model.to_owned()];
    if let Some(effort) = request
        .identity
        .params
        .effort
        .as_deref()
        .filter(|effort| !effort.is_empty())
    {
        argv.extend(["--effort".into(), effort.to_owned()]);
    }
    if let Some(session) = session {
        argv.extend(["--session".into(), session.clone()]);
    }
    argv
}

fn finalize_agent_process_stage(
    process: CompiledAgentProcess,
    request: &ExecRequest,
    managed_launch: Option<&crate::agents::ManagedLaunchState>,
    rimz_bin: &Path,
    runtime: &RuntimePaths,
) -> Result<AgentProcessStage, AgentProcessStageErr> {
    match &request.provider_account {
        ProviderAccountState::Unbound => Ok(AgentProcessStage::Ready(process)),
        ProviderAccountState::Pending { binding } => {
            let mut finalized = request.clone();
            finalized.provider_account = ProviderAccountState::Finalized {
                binding: binding.clone(),
            };
            let plan = plan_exec_argv(rimz_bin, runtime, &finalized)?;
            let argv = process.login_shell_argv(&plan.argv);
            if argv.is_empty() {
                return Err(AgentProcessStageErr::EmptyReentry);
            }
            Ok(AgentProcessStage::LoginShellReentry {
                process,
                argv,
                prompt_artifacts: plan.artifacts,
            })
        }
        ProviderAccountState::Finalized { binding } => {
            if managed_launch.and_then(crate::agents::ManagedLaunchState::binding) != Some(binding)
            {
                return Err(AgentProcessStageErr::FinalizedProviderMismatch);
            }
            let mut process = process;
            process.wrapping = ProcessWrapping::Direct;
            process.rebuild_argv();
            Ok(AgentProcessStage::Ready(process))
        }
    }
}

fn compose_agent_env(
    mut env: BTreeMap<String, String>,
    adapter: &crate::agents::AgentDefinition,
    request: &ExecRequest,
    extra_env: &BTreeMap<String, String>,
) -> AgentProcessResult<BTreeMap<String, String>> {
    for (key, value) in adapter.launch_env() {
        env.insert(key.to_owned(), value.to_owned());
    }
    env.extend(extra_env.clone());
    env.extend(exec_identity_env(request));
    if let Some(key) = invalid_env_key(&env) {
        return Err(AgentProcessCompileErr::InvalidEnvKey {
            kind: request.kind.to_string(),
            key: key.to_owned(),
        });
    }
    Ok(env)
}

/// Removes the inherited provider pair an outer launch stamped under `marker`, and the
/// marker itself, returning that pair. The pair is RimZ's only while its key still holds
/// the stamped value and is not a trusted `[[agents]]` env key; any other value is the
/// user's and passes through.
fn release_stamped_pair<'a>(
    env: &mut BTreeMap<String, String>,
    unset: &mut BTreeSet<String>,
    inherited: &'a BTreeMap<String, String>,
    secret_keys: &BTreeSet<String>,
    marker: &str,
) -> Option<(&'a str, &'a str)> {
    let owned = inherited
        .get(marker)
        .and_then(|stamp| stamp.split_once('='))
        .filter(|(key, value)| {
            !secret_keys.contains(*key) && inherited.get(*key).map(String::as_str) == Some(*value)
        });
    if let Some((key, _)) = owned {
        env.remove(key);
        unset.insert(key.to_owned());
    }
    env.remove(marker);
    unset.insert(marker.to_owned());
    owned
}

/// Sets a provider pair and stamps `marker` so a nested launch can tell it is RimZ's.
fn stamp_pair(
    env: &mut BTreeMap<String, String>,
    unset: &mut BTreeSet<String>,
    marker: &str,
    key: &str,
    value: &str,
) {
    env.insert(key.to_owned(), value.to_owned());
    unset.remove(key);
    env.insert(marker.to_owned(), format!("{key}={value}"));
    unset.remove(marker);
}

fn trusted_agent_env(
    project_root: &Path,
    kind: &str,
) -> AgentProcessResult<BTreeMap<String, String>> {
    use crate::trust::AgentEnv;

    match crate::trust::agent_env(project_root, kind)? {
        AgentEnv::Apply(env) => Ok(env),
        AgentEnv::Unconfigured => Ok(BTreeMap::new()),
        AgentEnv::Blocked(state) => Err(AgentProcessCompileErr::BlockedEnv {
            kind: kind.to_owned(),
            root: project_root.to_path_buf(),
            state: state.as_str(),
            fix: crate::trust::blocked_fix(state),
        }),
    }
}

/// Validate trust, provider capability, environment, and shell compilation
/// before any launch allocation or mux mutation.
/// Supply runtime paths for host isolation; settings artifacts are only planned.
pub fn preflight_agent_process(
    project_root: &Path,
    request: &ExecRequest,
    cwd: &Path,
    host_runtime: Option<&RuntimePaths>,
) -> AgentProcessResult<CompiledAgentProcess> {
    compile_agent_process(project_root, request, cwd, host_runtime)
}

/// Preflight one fresh provider launch before detailed pane identity exists.
pub fn preflight_agent_kind(project_root: &Path, kind: &str, cwd: &Path) -> AgentProcessResult<()> {
    preflight_agent_process(
        project_root,
        &ExecRequest::bare_launch(AgentKind::new_unchecked(kind), Vec::new()),
        cwd,
        None,
    )
    .map(drop)
}

/// Encode the request and launch prompt by artifact path, keeping mux argv bounded.
pub fn exec_argv(
    rimz_bin: &Path,
    runtime: &RuntimePaths,
    request: &ExecRequest,
) -> Result<Vec<String>, ExecWireErr> {
    let plan = plan_exec_argv(rimz_bin, runtime, request)?;
    for artifact in &plan.artifacts {
        write_prompt_artifact(artifact)?;
    }
    Ok(plan.argv)
}

struct ExecArgvPlan {
    argv: Vec<String>,
    artifacts: Vec<PromptArtifact>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct PromptArtifact {
    path: PathBuf,
    contents: String,
}

pub(super) fn write_prompt_artifact(artifact: &PromptArtifact) -> Result<(), ExecWireErr> {
    crate::disk::atomic::write_cache_bytes_atomically(&artifact.path, artifact.contents.as_bytes())
        .map_err(ExecWireErr::PromptWrite)
}

fn plan_exec_argv(
    rimz_bin: &Path,
    runtime: &RuntimePaths,
    request: &ExecRequest,
) -> Result<ExecArgvPlan, ExecWireErr> {
    let mut encoded = request.clone();
    encoded.identity.params.kind_ordinal = None;
    validate_exec_request(&encoded)?;
    let prompt_artifact = match &mut encoded.action {
        ExecAction::Launch { prompt, .. } => prompt.take().map(|contents| PromptArtifact {
            path: prompt_artifact_path(runtime, "task", &contents),
            contents,
        }),
        ExecAction::Pick { .. } | ExecAction::Resume { .. } | ExecAction::Fork { .. } => None,
    };
    let prompt_file = prompt_artifact
        .as_ref()
        .map(|artifact| artifact.path.clone());
    let mut artifacts: Vec<_> = prompt_artifact.into_iter().collect();
    for source in encoded
        .system_prompt_file
        .iter_mut()
        .chain(&mut encoded.append_system_prompt_files)
        .chain(
            encoded
                .team_prompt
                .iter_mut()
                .flat_map(|team| &mut team.files),
        )
    {
        if let crate::config::PromptSource::Text { text, .. } = source {
            let path = prompt_artifact_path(runtime, "frag", text);
            artifacts.push(PromptArtifact {
                path: path.clone(),
                contents: std::mem::take(text),
            });
            *source = crate::config::PromptSource::File(path);
        }
    }
    let mut argv = vec![
        rimz_bin.to_string_lossy().into_owned(),
        "agents".to_owned(),
        "exec".to_owned(),
        encoded.kind.to_string(),
    ];
    if let Some(path) = encoded.worktree_path.as_deref() {
        argv.extend([
            "--worktree-path".to_owned(),
            path.to_string_lossy().into_owned(),
        ]);
    }
    let payload = serde_json::to_string(&ExecWire {
        request: encoded,
        prompt_file,
    })
    .map_err(ExecWireErr::Serialize)?;
    let path = prompt_artifact_path(runtime, "exec", &payload).with_extension("json");
    argv.extend([
        "--request-file".to_owned(),
        path.to_string_lossy().into_owned(),
    ]);
    artifacts.push(PromptArtifact {
        path,
        contents: payload,
    });
    Ok(ExecArgvPlan { argv, artifacts })
}

/// Test-facing reader: check the visible envelope, then restore the launch
/// prompt from its artifact. Production decodes through
/// [`decode_exec_envelope`].
#[cfg(any(test, feature = "testkit"))]
pub fn decode_exec_request(
    visible_kind: &str,
    visible_worktree_path: Option<&Path>,
    payload: &str,
) -> Result<ExecRequest, ExecWireErr> {
    decode_exec_envelope(visible_kind, visible_worktree_path, payload)?.materialize()
}

/// Validate the complete visible envelope and request before any artifact I/O.
pub fn decode_exec_envelope(
    visible_kind: &str,
    visible_worktree_path: Option<&Path>,
    payload: &str,
) -> Result<ExecEnvelope, ExecWireErr> {
    let wire: ExecWire = serde_json::from_str(payload).map_err(ExecWireErr::Parse)?;
    let request = &wire.request;
    if request.kind != visible_kind {
        return Err(ExecWireErr::KindMismatch {
            visible: visible_kind.to_owned(),
            payload: request.kind.to_string(),
        });
    }
    if request.worktree_path.as_deref() != visible_worktree_path {
        return Err(ExecWireErr::WorktreeMismatch);
    }
    validate_exec_request(request)?;
    if wire.prompt_file.is_some() && !matches!(request.action, ExecAction::Launch { .. }) {
        return Err(ExecWireErr::Parse(serde::de::Error::custom(
            "prompt_file requires a launch action",
        )));
    }
    Ok(ExecEnvelope(wire))
}

fn validate_exec_request(request: &ExecRequest) -> Result<(), ExecWireErr> {
    if let Some(skills) = &request.skills {
        crate::config::validate_skill_list(skills)?;
    }
    if request.identity.launch_id.is_some()
        && request.identity.name.is_none()
        && !matches!(request.action, ExecAction::Resume { .. })
    {
        return Err(ExecWireErr::OrphanLaunchId);
    }
    if request.exit_on_run_completion && request.run_id.is_none() {
        return Err(ExecWireErr::MissingRunId);
    }
    if request.identity.params.kind_ordinal.is_some() {
        return Err(ExecWireErr::KindOrdinal);
    }
    if request
        .provider_account
        .binding()
        .is_some_and(|binding| binding.account_key().trim().is_empty())
    {
        return Err(ExecWireErr::EmptyProviderBinding);
    }
    Ok(())
}

/// The RIMZ_* identity env for one invocation (kind, run id, identity fields).
/// Callers merge trust env and adapter launch env around it.
fn exec_identity_env(request: &ExecRequest) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert(
        crate::harness::launch::ENV_AGENT_KIND.to_owned(),
        request.kind.to_string(),
    );
    // Always overwrite the ambient value. Resume records predating launch ids
    // deliberately export an empty value rather than inheriting their caller's
    // identity into the new pane.
    env.insert(
        crate::harness::launch::ENV_AGENT_ID.to_owned(),
        request.identity.launch_id.clone().unwrap_or_default(),
    );
    if let Some(run_id) = request.run_id.as_ref() {
        env.insert(
            crate::harness::launch::ENV_RUN_ID.to_owned(),
            run_id.to_string(),
        );
    }
    if let Some(name) = request.identity.name.as_ref() {
        env.insert(
            crate::harness::launch::ENV_AGENT_NAME.to_owned(),
            name.clone(),
        );
    }
    for field in &LAUNCH_FIELDS {
        if let Some(value) = (field.encode)(&request.identity.params) {
            env.insert(field.env.to_owned(), value);
        }
    }
    env
}

/// Fills each unset launch parameter from its identity env var, the inverse
/// of `exec_identity_env`. `identity_env` owns lookup and validation; a
/// malformed ordinal stays unset.
pub fn fill_launch_identity_env(
    params: &mut crate::agents::LaunchParams,
    mut identity_env: impl FnMut(&'static str) -> Option<String>,
) {
    for field in &LAUNCH_FIELDS {
        match (field.target)(params) {
            LaunchFieldValue::Text(value) if value.is_none() => *value = identity_env(field.env),
            LaunchFieldValue::Ordinal(value) if value.is_none() => {
                *value = identity_env(field.env).and_then(|raw| raw.parse::<u32>().ok());
            }
            LaunchFieldValue::Text(_) | LaunchFieldValue::Ordinal(_) => {}
        }
    }
    // Epoch 0 is how a launch without one writes its key: read it as none, so
    // the fold never takes it for a swap's reservation.
    params.epoch = params.epoch.filter(|&epoch| epoch > 0);
}

/// Process environment after applying RimZ's launch overrides. Non-Unicode
/// ambient values stay inherited by the child but cannot affect Qwen's textual
/// provider selection.
fn effective_launch_env(overrides: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut env = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect::<BTreeMap<_, _>>();
    env.extend(overrides.clone());
    env
}

/// Wrap an agent command in the launch shell's startup path so shell rc
/// env applies, while RimZ's launch env is re-applied after rc processing.
///
/// Launch env is encoded as `KEY=VALUE` argv entries for `/usr/bin/env`, which
/// makes those values visible to same-user process inspection during startup.
/// The inputs today are trusted project config, adapter pins, and run ids; a
/// future secret-bearing launch source needs a two-stage re-exec channel that
/// does not place assignments in argv.
fn login_shell_argv_with(
    shell: Option<&Path>,
    env_bin_available: bool,
    env: &BTreeMap<String, String>,
    unset: &BTreeSet<String>,
    agent_argv: &[String],
) -> Vec<String> {
    let Some(shell) = shell else {
        tracing::debug!("agent launch shell wrapper disabled: no launchable user shell");
        return agent_argv.to_vec();
    };
    if !env_bin_available {
        tracing::debug!(
            env_bin = ENV_BIN,
            "agent launch shell wrapper disabled: env binary missing",
        );
        return agent_argv.to_vec();
    }
    if let Some(key) = invalid_env_key(env).or_else(|| {
        unset
            .iter()
            .find(|key| !valid_env_key(key))
            .map(String::as_str)
    }) {
        tracing::debug!(
            key,
            "agent launch shell wrapper disabled: launch env key cannot be represented as an env(1) assignment",
        );
        return agent_argv.to_vec();
    }

    match ShellFamily::from_shell(shell) {
        ShellFamily::Bash => bash_interactive_shell_argv(shell, env, unset, agent_argv),
        ShellFamily::Posix => posix_login_shell_argv(shell, env, unset, agent_argv),
        ShellFamily::Fish => fish_login_shell_argv(shell, env, unset, agent_argv),
        ShellFamily::Csh => {
            tracing::debug!(
                shell = %shell.display(),
                "agent launch shell wrapper disabled: csh-style shells need a dedicated argv grammar",
            );
            agent_argv.to_vec()
        }
    }
}

fn bash_interactive_shell_argv(
    shell: &Path,
    env: &BTreeMap<String, String>,
    unset: &BTreeSet<String>,
    agent_argv: &[String],
) -> Vec<String> {
    let mut argv = vec![
        shell.to_string_lossy().into_owned(),
        "-i".to_owned(),
        "-c".to_owned(),
        POSIX_LOGIN_SHELL_SCRIPT.to_owned(),
        POSIX_ARG0.to_owned(),
    ];
    argv.extend(env_arguments(env, unset));
    argv.extend(agent_argv.iter().cloned());
    argv
}

fn posix_login_shell_argv(
    shell: &Path,
    env: &BTreeMap<String, String>,
    unset: &BTreeSet<String>,
    agent_argv: &[String],
) -> Vec<String> {
    let mut argv = vec![
        shell.to_string_lossy().into_owned(),
        "-l".to_owned(),
        "-i".to_owned(),
        "-c".to_owned(),
        POSIX_LOGIN_SHELL_SCRIPT.to_owned(),
        POSIX_ARG0.to_owned(),
    ];
    argv.extend(env_arguments(env, unset));
    argv.extend(agent_argv.iter().cloned());
    argv
}

fn fish_login_shell_argv(
    shell: &Path,
    env: &BTreeMap<String, String>,
    unset: &BTreeSet<String>,
    agent_argv: &[String],
) -> Vec<String> {
    let mut argv = vec![
        shell.to_string_lossy().into_owned(),
        "-l".to_owned(),
        "-i".to_owned(),
        "-c".to_owned(),
        FISH_LOGIN_SHELL_SCRIPT.to_owned(),
    ];
    argv.extend(env_arguments(env, unset));
    argv.extend(agent_argv.iter().cloned());
    argv
}

fn env_arguments<'a>(
    env: &'a BTreeMap<String, String>,
    unset: &'a BTreeSet<String>,
) -> impl Iterator<Item = String> + 'a {
    unset
        .iter()
        .flat_map(|key| ["-u".to_owned(), key.clone()])
        .chain(env.iter().map(|(key, value)| format!("{key}={value}")))
}

fn valid_env_key(key: &str) -> bool {
    !key.is_empty() && !key.contains('=') && !key.starts_with('-')
}

fn invalid_env_key(env: &BTreeMap<String, String>) -> Option<&str> {
    env.keys()
        .find(|key| !valid_env_key(key))
        .map(String::as_str)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShellFamily {
    Bash,
    Posix,
    Fish,
    Csh,
}

impl ShellFamily {
    fn from_shell(shell: &Path) -> Self {
        let Some(name) = shell.file_name().and_then(|name| name.to_str()) else {
            return Self::Posix;
        };
        match name {
            "bash" => Self::Bash,
            "fish" => Self::Fish,
            "csh" | "tcsh" => Self::Csh,
            "zsh" | "sh" | "dash" | "ksh" | "mksh" | "ash" => Self::Posix,
            _ => Self::Posix,
        }
    }
}

#[cfg(test)]
mod tests;
