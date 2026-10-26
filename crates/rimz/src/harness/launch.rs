//! Provider-process compilation, exec-wrapper argv, and login-shell launch policy.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::agents::capabilities::SystemTextChannel;
use crate::disk::paths::RuntimePaths;
use crate::harness::launch_reminders::LaunchReminders;
use crate::harness::prompt_compose::{TEXT_PROMPT_LIMIT, prompt_artifact_path};
use crate::harness::spec::AgentCell;
use crate::ids::{AgentKind, MessageId, RunId};

const ENV_BIN: &str = "/usr/bin/env";
const POSIX_LOGIN_SHELL_SCRIPT: &str = r#"exec /usr/bin/env "$@""#;
const FISH_LOGIN_SHELL_SCRIPT: &str = "exec /usr/bin/env $argv";
const POSIX_ARG0: &str = "rimz-agent-launch";
const ENV_PROBE_SH: &str = "/bin/sh";
const ENV_PROBE_START: &str = "__RIMZ_ENV_PROBE_START__";
const ENV_PROBE_END: &str = "__RIMZ_ENV_PROBE_END__";
const ENV_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const ENV_PROBE_POLL: Duration = Duration::from_millis(25);
static ENV_PROBE_COUNTER: AtomicU64 = AtomicU64::new(0);

pub const ENV_RUN_ID: &str = "RIMZ_RUN_ID";
/// Launch reminders for an adapter whose RimZ extension injects them
/// (`SystemTextChannel::ExtensionEnv`). Always exported, empty when there is no
/// reminder, so a parent's value never leaks into a child launch.
const ENV_LAUNCH_REMINDERS: &str = "RIMZ_LAUNCH_REMINDERS";
/// `1` when RimZ's extension requests priority serving for this launch;
/// cleared on every other launch so a nested launch never inherits it.
const ENV_LAUNCH_FAST: &str = "RIMZ_LAUNCH_FAST";
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
/// Claude Code's own shell override, stamped beside `SHELL` under a configured
/// `[agents] shell` so the provider's tool shell is the wrapper's.
const ENV_CLAUDE_CODE_SHELL: &str = "CLAUDE_CODE_SHELL";
const AGENT_SHELL_FIX: &str = "set it to the absolute path of an installed bash, zsh, fish or sh-family shell, or remove the line from the per-machine config";

/// A configured `[agents] shell` that cannot carry an agent launch.
#[derive(Debug, thiserror::Error)]
#[error("[agents] shell = \"{}\" {reason}; {AGENT_SHELL_FIX}", shell.display())]
pub struct AgentShellErr {
    shell: PathBuf,
    reason: &'static str,
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
            Some("rimz config set agents.shell \"$(command -v bash)\"".into()),
            crate::blocker::Surface::Plain,
        )
    }
}

/// Refuse a configured `[agents] shell` the launch wrapper cannot run under.
/// A relative name would resolve through a PATH other than the one checked
/// here, and a csh-family shell would silently skip the wrapper.
pub fn validate_agent_shell(shell: &Path) -> Result<(), AgentShellErr> {
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
        reason,
    })
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
pub enum ProgramLookupErr {
    #[error(
        "launch env key `{0}` is invalid; environment variable names must be non-empty, cannot contain `=`, and cannot start with `-`"
    )]
    InvalidEnvKey(String),
    #[error("launch environment probe produced an empty command")]
    EmptyProbeCommand,
    #[error("running launch environment probe `{program}`: {source}")]
    ProbeSpawn {
        program: String,
        #[source]
        source: io::Error,
    },
    #[error("waiting for launch environment probe `{program}`: {source}")]
    ProbeWait {
        program: String,
        #[source]
        source: io::Error,
    },
    #[error("launch environment probe `{program}` timed out after {timeout:?}")]
    ProbeTimeout { program: String, timeout: Duration },
    #[error("cannot access launch environment probe output {path}: {source}")]
    ProbeIo {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("launch environment probe exited with {0}")]
    ProbeStatus(ExitStatus),
    #[error("launch environment probe wrote non-UTF-8 output: {0}")]
    ProbeUtf8(#[from] std::string::FromUtf8Error),
    #[error("launch environment probe output did not contain its PATH markers")]
    ProbeOutput,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentProcessCompileErr {
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
    #[error("agent `{kind}` has no headless launch command")]
    NoHeadless { kind: String },
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
    pub(super) settings_artifact: Option<crate::agents::skills::LaunchSettingsArtifact>,
    /// Provider command before shell startup wrapping.
    pub provider_argv: Vec<String>,
    /// Provider executable used by PATH preflight.
    pub provider_program: String,
    /// Final shell-wrapped command.
    pub argv: Vec<String>,
    /// Final child environment, also re-applied after shell startup.
    pub env: BTreeMap<String, String>,
    pub secret_keys: BTreeSet<String>,
    pub reminder: String,
    /// Environment keys removed before execution and again after shell startup.
    pub unset: BTreeSet<String>,
    /// The one shell every wrapper and the PATH probe of this launch run under.
    shell: Option<PathBuf>,
    wrapping: ProcessWrapping,
    session_step_argv: Vec<String>,
}

impl CompiledAgentProcess {
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

    /// Resolve the provider program in the PATH this launch will see after
    /// its shell's startup files and RimZ's launch env are applied.
    pub fn resolve_program_after_shell_rc(&self) -> Result<Option<PathBuf>, ProgramLookupErr> {
        resolve_program_with(
            self.shell.as_deref(),
            Path::new(ENV_BIN).is_file(),
            &self.env,
            &self.provider_program,
        )
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
        env: crate::workspace::ENV_CHANNEL,
        encode: |params| params.channel.clone(),
        target: |params| LaunchFieldValue::Text(&mut params.channel),
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
    /// Whether the applied tools list excludes `Skill`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_skill_tool: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<crate::config::ToolRule>>,
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
    /// The `### Loop` or headless `### Judge` reminder body composed for this launch; set on a resident's prompt leader, a loop single run, or a judge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_reminder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headless: Option<crate::agents::HeadlessRequest>,
    #[serde(default)]
    pub identity: ExecIdentity,
}

impl ExecRequest {
    /// The skills callable with the launch's applied tools and profile policy.
    pub fn callable_skills(&self) -> Option<&[crate::config::SkillName]> {
        if self.no_skill_tool {
            Some(&[])
        } else {
            self.skills.as_deref()
        }
    }

    pub(super) fn launch_skills(
        &self,
    ) -> (Option<Vec<crate::config::SkillName>>, Option<&'static str>) {
        let withheld = self.subagent.then_some("rimz-subagents");
        let listed = self.skills.as_ref().map(|skills| {
            skills
                .iter()
                .filter(|skill| Some(skill.as_str()) != withheld)
                .cloned()
                .collect()
        });
        (listed, withheld)
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
            no_skill_tool: cell.no_skill_tool,
            allowed_tools: cell.allowed_tools.clone(),
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
            no_skill_tool: false,
            allowed_tools: None,
            isolation_default: None,
            provider_account: ProviderAccountState::Unbound,
            run_id: None,
            worktree_path: None,
            close_pane_on_exit: false,
            exit_on_run_completion: false,
            subagent: false,
            follow_up_message: None,
            loop_reminder: None,
            headless: None,
            identity: ExecIdentity::default(),
        }
    }
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
    compile_provider_argv_with_headless(adapter, kind, action, cwd, None, (cwd, &mut None))
}

fn compile_provider_argv_with_headless(
    adapter: &crate::agents::AgentDefinition,
    kind: &str,
    action: &ExecAction,
    cwd: &Path,
    headless: Option<&crate::agents::HeadlessRequest>,
    settings: (
        &Path,
        &mut Option<crate::agents::skills::LaunchSettingsArtifact>,
    ),
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
        match (headless, action) {
            (Some(request), ExecAction::Launch { prompt, extra_args }) => adapter
                .spec()
                .launch
                .headless
                .ok_or_else(|| AgentProcessCompileErr::NoHeadless {
                    kind: kind.to_owned(),
                })?
                .render_argv(extra_args, prompt.as_deref(), request, cwd, settings)?,
            (Some(_), _) => {
                return Err(AgentProcessCompileErr::NoHeadless {
                    kind: kind.to_owned(),
                });
            }
            (None, ExecAction::Launch { prompt, extra_args }) => adapter
                .launch_command(extra_args, prompt.as_deref())
                .ok_or_else(|| AgentProcessCompileErr::NoLaunch {
                    kind: kind.to_owned(),
                })?,
            (None, ExecAction::Pick { extra_args }) => {
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
            (
                None,
                ExecAction::Resume {
                    session_id,
                    extra_args,
                },
            ) => {
                let mut argv = adapter.resume_command(session_id, cwd).ok_or_else(|| {
                    AgentProcessCompileErr::NoResume {
                        kind: kind.to_owned(),
                    }
                })?;
                argv.extend(extra_args.iter().cloned());
                argv
            }
            (
                None,
                ExecAction::Fork {
                    session_id,
                    extra_args,
                },
            ) => {
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
            validate_agent_shell(configured)?;
            let env = agent_shell_env(configured, user_shell.as_deref());
            (Some(configured.to_path_buf()), env)
        }
        None => (user_shell, BTreeMap::new()),
    };
    let trusted_env = trusted_agent_env(project_root, kind)?;
    let secret_keys = trusted_env.keys().cloned().collect();
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
    let env_warnings = adapter.finalize_launch_env(
        &login_env,
        &mut env,
        reminders.disable_native_subagents(request),
        reminders.system_prompt_file.as_deref(),
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
    if reminders.lsp_configured
        && crate::config::skill_is_callable(request.callable_skills(), "rimz-lsp")
    {
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
        adapter.configure_room_permissions(
            action.extra_args_mut(),
            &login_env,
            (
                cwd,
                linked.as_ref().map(|worktree| worktree.primary.as_path()),
            ),
            roots,
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
    let (listed, withheld) = request.launch_skills();
    let (host_skills, mut settings_artifact) = if let Some(runtime) = host_runtime
        && (listed.is_some()
            || (withheld.is_some()
                && matches!(
                    adapter.spec().host_skills,
                    crate::agents::skills::HostSkills::Switch { .. }
                ))) {
        let artifact_dir = runtime.prompt_dir();
        let mut args = action.extra_args().to_vec();
        match adapter.spec().host_skills.apply(
            (listed.as_deref(), withheld),
            adapter.skills_home(&skill_env).as_deref(),
            crate::disk::paths::skills_library_in(&skill_env).as_deref(),
            (cwd, &artifact_dir),
            &mut args,
        ) {
            Ok((plan, artifact)) => {
                *action.extra_args_mut() = args;
                (Some(plan), artifact)
            }
            Err(error) if listed.is_none() => {
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
    if let (Some(listed), Some((artifact_dir, _))) = (&listed, &reminders.settings) {
        adapter.allow_listed_skill_args(
            listed,
            (cwd, artifact_dir),
            action.extra_args_mut(),
            &mut settings_artifact,
        )?;
    }
    if let (Some(rules), Some((artifact_dir, _))) = (&request.allowed_tools, &reminders.settings)
        && let crate::agents::skills::ToolRules::Settings { render } = adapter.spec().tool_rules
    {
        render(
            rules,
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
                crate::disk::paths::skills_library_in(&skill_env).as_deref(),
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
    if let Some(run) = &reminders.team_notes_run {
        adapter.allow_team_notes_args(
            run,
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
    if fast_settings {
        adapter.merge_fast_settings(
            (cwd, artifact_dir),
            action.extra_args_mut(),
            &mut settings_artifact,
        )?;
    }
    let provider_argv = compile_provider_argv_with_headless(
        adapter,
        kind,
        &action,
        cwd,
        request.headless.as_ref(),
        (artifact_dir, &mut settings_artifact),
    )?;
    let provider_program =
        provider_argv
            .first()
            .cloned()
            .ok_or_else(|| AgentProcessCompileErr::EmptyCommand {
                kind: kind.to_owned(),
            })?;
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
    if request.identity.params.fast
        && adapter.spec().launch.presets.fast == crate::agents::definition::FastSwitch::ExtensionEnv
    {
        env.insert(ENV_LAUNCH_FAST.to_owned(), "1".to_owned());
    } else {
        env.remove(ENV_LAUNCH_FAST);
        unset.insert(ENV_LAUNCH_FAST.to_owned());
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
    let mut process = CompiledAgentProcess {
        host_skills,
        env_warnings,
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

/// The argv shape a channel occupies; the extension channel has none.
fn argv_matcher(channel: &SystemTextChannel) -> Option<crate::agents::PresetArgMatcher> {
    use crate::agents::PresetArgMatcher;

    match channel {
        SystemTextChannel::TextFlag { flags } => Some(PresetArgMatcher::TextFlag(flags.clone())),
        SystemTextChannel::ConfigKey { flags, key } => Some(PresetArgMatcher::ConfigKey {
            flags: flags.clone(),
            key: key.clone(),
        }),
        SystemTextChannel::ExtensionEnv => None,
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
        SystemTextChannel::TextFlag { .. } | SystemTextChannel::ExtensionEnv => None,
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
        SystemTextChannel::ExtensionEnv => None,
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
    process.session_step_argv = model_session_step_argv(request, rimz_bin);
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

fn model_session_step_argv(request: &ExecRequest, rimz_bin: &Path) -> Vec<String> {
    if request.headless.is_some()
        || matches!(
            request.provider_account,
            ProviderAccountState::Pending { .. }
        )
    {
        return Vec::new();
    }
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
    let mut argv = vec![
        rimz_bin.display().to_string(),
        "agents".into(),
        "session-step".into(),
        request.kind.to_string(),
        "--model".into(),
        model.to_owned(),
    ];
    if let Some(session) = session {
        argv.extend(["--session".into(), session.clone()]);
    }
    argv.push("--".into());
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

/// Encode the launch prompt by artifact path, keeping its body out of mux argv.
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
    argv.extend(["--request".to_owned(), payload]);
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

fn resolve_program_with(
    shell: Option<&Path>,
    env_bin_available: bool,
    env: &BTreeMap<String, String>,
    program: &str,
) -> Result<Option<PathBuf>, ProgramLookupErr> {
    let Some(path) = final_launch_path_with(shell, env_bin_available, env)? else {
        return Ok(None);
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    Ok(which::which_in(program, Some(path), cwd).ok())
}

fn final_launch_path_with(
    shell: Option<&Path>,
    env_bin_available: bool,
    env: &BTreeMap<String, String>,
) -> Result<Option<String>, ProgramLookupErr> {
    if let Some(key) = invalid_env_key(env) {
        return Err(ProgramLookupErr::InvalidEnvKey(key.to_owned()));
    }
    if !env_bin_available {
        return Ok(direct_launch_path(env));
    }
    let argv = login_shell_argv_with(
        shell,
        env_bin_available,
        env,
        &BTreeSet::new(),
        &env_probe_argv(),
    );
    let (program, rest) = argv
        .split_first()
        .ok_or(ProgramLookupErr::EmptyProbeCommand)?;
    let (status, stdout) = run_probe_command(program, rest, env)?;
    if !status.success() {
        return Err(ProgramLookupErr::ProbeStatus(status));
    }
    let stdout = String::from_utf8(stdout)?;
    parse_probe_path(&stdout)
}

fn run_probe_command(
    program: &str,
    rest: &[String],
    env: &BTreeMap<String, String>,
) -> Result<(ExitStatus, Vec<u8>), ProgramLookupErr> {
    let stdout_path = env_probe_stdout_path();
    let stdout_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&stdout_path)
        .map_err(|source| ProgramLookupErr::ProbeIo {
            path: stdout_path.clone(),
            source,
        })?;
    let spawn = Command::new(program)
        .args(rest)
        .envs(env)
        .stdout(stdout_file)
        .stderr(std::process::Stdio::null())
        .spawn();
    let mut child = match spawn {
        Ok(child) => child,
        Err(source) => {
            remove_probe_stdout(&stdout_path);
            return Err(ProgramLookupErr::ProbeSpawn {
                program: program.to_owned(),
                source,
            });
        }
    };
    let timeout = env_probe_timeout();
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child
            .try_wait()
            .map_err(|source| ProgramLookupErr::ProbeWait {
                program: program.to_owned(),
                source,
            })? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                remove_probe_stdout(&stdout_path);
                return Err(ProgramLookupErr::ProbeTimeout {
                    program: program.to_owned(),
                    timeout,
                });
            }
            None => std::thread::sleep(ENV_PROBE_POLL),
        }
    };
    let stdout = match std::fs::read(&stdout_path) {
        Ok(stdout) => stdout,
        Err(source) => {
            remove_probe_stdout(&stdout_path);
            return Err(ProgramLookupErr::ProbeIo {
                path: stdout_path.clone(),
                source,
            });
        }
    };
    remove_probe_stdout(&stdout_path);
    Ok((status, stdout))
}

fn env_probe_timeout() -> Duration {
    #[cfg(any(test, feature = "testkit"))]
    if let Some(ms) = std::env::var("RIMZ_TEST_ENV_PROBE_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
    {
        return Duration::from_millis(ms);
    }
    ENV_PROBE_TIMEOUT
}

fn env_probe_stdout_path() -> PathBuf {
    let counter = ENV_PROBE_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("rimz-env-probe-{}-{counter}", std::process::id()))
}

fn remove_probe_stdout(path: &Path) {
    let _ = std::fs::remove_file(path);
}

fn direct_launch_path(env: &BTreeMap<String, String>) -> Option<String> {
    env.get("PATH")
        .cloned()
        .or_else(|| std::env::var("PATH").ok())
}

fn env_probe_argv() -> Vec<String> {
    vec![
        ENV_PROBE_SH.to_owned(),
        "-c".to_owned(),
        format!("printf '%s\\n' {ENV_PROBE_START}; {ENV_BIN}; printf '%s\\n' {ENV_PROBE_END}"),
    ]
}

fn parse_probe_path(output: &str) -> Result<Option<String>, ProgramLookupErr> {
    let mut saw_start = false;
    let mut path = None;
    for line in output.lines() {
        if line == ENV_PROBE_START {
            saw_start = true;
            continue;
        }
        if line == ENV_PROBE_END {
            return saw_start
                .then_some(path)
                .ok_or(ProgramLookupErr::ProbeOutput);
        }
        if !saw_start {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key == "PATH"
        {
            path = Some(value.to_owned());
        }
    }
    Err(ProgramLookupErr::ProbeOutput)
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
