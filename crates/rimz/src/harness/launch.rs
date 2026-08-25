//! Provider-process compilation, exec-wrapper argv, and login-shell launch policy.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::ids::{AgentKind, RunId, WorkspaceId};

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
/// The `[agents.teams]` team name an agent launched under. Set by the launch
/// wrapper; read by member CLI calls so in-place teams scope to their channel.
pub const ENV_TEAM: &str = "RIMZ_TEAM";
/// The inline multi-agent launch cohort this agent belongs to. Team launches
/// use [`ENV_TEAM`] as their cohort key; inline layouts use this generated id.
pub const ENV_LAUNCH_GROUP: &str = "RIMZ_LAUNCH_GROUP";
/// The agent's order inside its launch cohort: team role-list index or inline
/// agent-cell index. Set by the wrapper; read into lifecycle observations.
pub const ENV_LAUNCH_ORDINAL: &str = "RIMZ_LAUNCH_ORDINAL";
/// Named cooperation lane an agent launched under. Set by the launch wrapper;
/// read by lifecycle hooks and peer-message commands as the routing channel.
pub const ENV_CHANNEL: &str = "RIMZ_CHANNEL";
/// The cwd backing a launched pane. Set with the room pin so split panes can
/// still report the worktree path they were opened for.
pub const ENV_WORKTREE_PATH: &str = "RIMZ_WORKTREE_PATH";
/// The model selected by launch flags or profile presets. Set by the launch
/// wrapper; read into the lifecycle observation as card identity fallback.
pub const ENV_AGENT_MODEL: &str = "RIMZ_AGENT_MODEL";
/// The reasoning effort selected by launch flags or profile presets. Set by
/// the launch wrapper; read into the lifecycle observation as card identity fallback.
pub const ENV_AGENT_EFFORT: &str = "RIMZ_AGENT_EFFORT";
/// The canonical dollar cap selected by launch flags, profiles, or roles.
/// Set by the launch wrapper and read into lifecycle observations.
pub const ENV_AGENT_BUDGET: &str = "RIMZ_AGENT_BUDGET";
/// The configured `[harness] rtk` mode (`auto`/`on`/`off`), exported to every
/// agent launch so `cargo xtask` can route recognized cargo commands through
/// `rtk`. Read by xtask, never by rimz itself.
pub(super) const ENV_RTK: &str = "RIMZ_RTK";

pub const SUBAGENT_REMINDER: &str = concat!(
    "<system_reminder>You are a subagent: a supervised child launched by another agent to ",
    "complete the task you were given. You must not spawn agents or subagents of any kind — do not use ",
    "agent, task, or spawn tools, and do not launch `rimz subagents`, `rimz agents`, or ",
    "`rimz teams`. Do the work yourself with your direct tools and report the result; your final ",
    "message is returned to your caller when you exit.</system_reminder>"
);

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
    #[error("launch environment probe timed out after {0:?}")]
    ProbeTimeout(Duration),
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
    #[error("unknown agent kind `{kind}`")]
    UnknownAgent { kind: String },
    #[error("agent `{kind}` has no launch command")]
    NoLaunch { kind: String },
    #[error("agent `{kind}` has no resume command")]
    NoResume { kind: String },
    #[error("agent `{kind}` has no fork command")]
    NoFork { kind: String },
    #[error("agent `{kind}` produced an empty launch command")]
    EmptyCommand { kind: String },
    #[error(transparent)]
    Trust(#[from] crate::trust::TrustErr),
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

pub type AgentProcessResult<T> = std::result::Result<T, AgentProcessCompileErr>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompiledAgentProcess {
    /// Provider command before shell startup wrapping.
    provider_argv: Vec<String>,
    /// Provider executable used by PATH preflight.
    pub provider_program: String,
    /// Final shell-wrapped command.
    pub argv: Vec<String>,
    /// Final child environment, also re-applied after shell startup.
    pub env: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentProcessStage {
    Ready(CompiledAgentProcess),
    LoginShellReentry {
        process: CompiledAgentProcess,
        argv: Vec<String>,
    },
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

impl AgentProcessStageErr {
    pub fn is_finalized_provider_mismatch(&self) -> bool {
        matches!(self, Self::FinalizedProviderMismatch)
    }
}

/// Resolve the user's configured shell for launches that should match a
/// normal terminal. `$SHELL` wins when it names a launchable shell; otherwise
/// the passwd entry is used. Sentinels such as `nologin` and missing absolute
/// paths are rejected.
pub fn user_shell() -> Option<PathBuf> {
    match env_shell() {
        Some(shell) => launchable_shell(&shell).then_some(shell),
        None => passwd_shell().filter(|shell| launchable_shell(shell)),
    }
}

/// Resolve the user's shell program for an ordinary shell pane.
pub fn user_shell_program() -> String {
    user_shell()
        .unwrap_or_else(|| PathBuf::from("sh"))
        .to_string_lossy()
        .into_owned()
}

/// Short, stable process identity for a pane title.
pub fn pane_short_name(argv: &[String]) -> Option<String> {
    if argv.get(1).is_some_and(|arg| arg == "agents")
        && argv.get(2).is_some_and(|arg| arg == "exec")
    {
        return argv.get(3).filter(|kind| !kind.is_empty()).cloned();
    }
    Path::new(argv.first()?)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
}

/// Shell pane argv for an empty named channel, pinned to the room identity.
pub fn channel_shell_argv(
    workspace_id: &WorkspaceId,
    project_root: &Path,
    worktree_path: &Path,
    channel: &str,
) -> Vec<String> {
    vec![
        "env".to_owned(),
        "RIMZ=1".to_owned(),
        format!("{}={workspace_id}", crate::workspace::ENV_WORKSPACE_ID),
        format!(
            "{}={}",
            crate::workspace::ENV_PROJECT_ROOT,
            project_root.display()
        ),
        format!(
            "{}={}",
            crate::harness::launch::ENV_WORKTREE_PATH,
            worktree_path.display()
        ),
        format!("{}={channel}", crate::harness::launch::ENV_CHANNEL),
        user_shell_program(),
    ]
}

/// Shell pane argv for a resume tab label, falling back to a plain shell.
pub fn channel_label_shell_argv(
    workspace_id: &WorkspaceId,
    project_root: &Path,
    worktree_path: &Path,
    label: &str,
) -> Vec<String> {
    let Some(channel) = label.strip_prefix('#').filter(|value| !value.is_empty()) else {
        return vec![user_shell_program()];
    };
    channel_shell_argv(workspace_id, project_root, worktree_path, channel)
}

/// The identity a `rimz agents exec` pane carries in its structured request
/// and as RIMZ_* env (crate::harness::launch::ENV_*) for lifecycle hooks and peer
/// attribution.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecIdentity {
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

#[derive(Clone, Copy)]
enum LaunchFieldValue<'a> {
    Text(Option<&'a str>),
    Mode(Option<crate::agents::PermissionMode>),
    Ordinal(Option<u32>),
}

impl LaunchFieldValue<'_> {
    fn into_env(self) -> Option<String> {
        match self {
            Self::Text(value) => value.map(ToOwned::to_owned),
            Self::Mode(value) => value.map(|value| value.to_string()),
            Self::Ordinal(value) => value.map(|value| value.to_string()),
        }
    }
}

#[derive(Clone, Copy)]
struct LaunchField<'a> {
    env: Option<&'static str>,
    value: LaunchFieldValue<'a>,
}

fn launch_fields(params: &crate::agents::LaunchParams) -> [LaunchField<'_>; 10] {
    let text = |field: fn(&crate::agents::LaunchParams) -> Option<&str>| {
        LaunchFieldValue::Text(field(params))
    };
    [
        LaunchField {
            env: Some(crate::harness::launch::ENV_AGENT_PROFILE),
            value: text(|params| params.profile.as_deref()),
        },
        LaunchField {
            env: None,
            value: LaunchFieldValue::Mode(params.mode),
        },
        LaunchField {
            env: Some(crate::harness::launch::ENV_AGENT_ROLE),
            value: text(|params| params.role.as_deref()),
        },
        LaunchField {
            env: Some(crate::harness::launch::ENV_TEAM),
            value: text(|params| params.team.as_deref()),
        },
        LaunchField {
            env: Some(crate::harness::launch::ENV_LAUNCH_GROUP),
            value: text(|params| params.launch_group.as_deref()),
        },
        LaunchField {
            env: Some(crate::harness::launch::ENV_LAUNCH_ORDINAL),
            value: LaunchFieldValue::Ordinal(params.launch_ordinal),
        },
        LaunchField {
            env: Some(crate::harness::launch::ENV_CHANNEL),
            value: text(|params| params.channel.as_deref()),
        },
        LaunchField {
            env: Some(crate::harness::launch::ENV_AGENT_MODEL),
            value: text(|params| params.model.as_deref()),
        },
        LaunchField {
            env: Some(crate::harness::launch::ENV_AGENT_EFFORT),
            value: text(|params| params.effort.as_deref()),
        },
        LaunchField {
            env: Some(crate::harness::launch::ENV_AGENT_BUDGET),
            value: text(|params| params.budget.as_deref()),
        },
    ]
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ExecAction {
    Launch {
        prompt: Option<String>,
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
    pub fn extra_args(&self) -> &[String] {
        match self {
            Self::Launch { extra_args, .. }
            | Self::Resume { extra_args, .. }
            | Self::Fork { extra_args, .. } => extra_args,
        }
    }

    pub fn extra_args_mut(&mut self) -> &mut Vec<String> {
        match self {
            Self::Launch { extra_args, .. }
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
    pub fn binding(&self) -> Option<&crate::agents::ProviderAccountBinding> {
        match self {
            Self::Unbound => None,
            Self::Pending { binding } | Self::Finalized { binding } => Some(binding),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecRequest {
    pub kind: AgentKind,
    pub action: ExecAction,
    #[serde(default)]
    pub system_prompt_file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub append_system_prompt_files: Vec<PathBuf>,
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
    #[serde(default)]
    pub identity: ExecIdentity,
}

impl ExecRequest {
    pub fn bare_launch(kind: AgentKind, extra_args: Vec<String>) -> Self {
        Self {
            kind,
            action: ExecAction::Launch {
                prompt: None,
                extra_args,
            },
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            provider_account: ProviderAccountState::Unbound,
            run_id: None,
            worktree_path: None,
            close_pane_on_exit: false,
            exit_on_run_completion: false,
            subagent: false,
            identity: ExecIdentity::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExecWireErr {
    #[error("serializing hidden agent exec request: {0}")]
    Serialize(#[source] serde_json::Error),
    #[error("parsing hidden agent exec request: {0}")]
    Parse(#[source] serde_json::Error),
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
    let argv = match action {
        ExecAction::Launch { prompt, extra_args } => adapter
            .launch_command(extra_args, prompt.as_deref())
            .ok_or_else(|| AgentProcessCompileErr::NoLaunch {
                kind: kind.to_owned(),
            })?,
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
pub fn compile_agent_process(
    project_root: &Path,
    rtk: crate::config::RtkMode,
    request: &ExecRequest,
    cwd: &Path,
) -> AgentProcessResult<CompiledAgentProcess> {
    compile_agent_process_with_extra_env(project_root, rtk, request, cwd, &BTreeMap::new())
}

fn compile_agent_process_with_extra_env(
    project_root: &Path,
    rtk: crate::config::RtkMode,
    request: &ExecRequest,
    cwd: &Path,
    extra_env: &BTreeMap<String, String>,
) -> AgentProcessResult<CompiledAgentProcess> {
    let kind = request.kind.as_str();
    let adapter = crate::agents::find_definition(kind).ok_or_else(|| {
        AgentProcessCompileErr::UnknownAgent {
            kind: kind.to_owned(),
        }
    })?;
    let mut action = request.action.clone();
    if request.subagent {
        adapter.lockdown_subagent_args(action.extra_args_mut());
        if let Some(append_args) = adapter.append_system_text_args(SUBAGENT_REMINDER) {
            merge_appended_system_text(action.extra_args_mut(), append_args);
        }
    }
    let provider_argv = compile_provider_argv(adapter, kind, &action, cwd)?;
    let provider_program =
        provider_argv
            .first()
            .cloned()
            .ok_or_else(|| AgentProcessCompileErr::EmptyCommand {
                kind: kind.to_owned(),
            })?;
    let env = compose_agent_env(
        trusted_agent_env(project_root, kind)?,
        adapter,
        rtk,
        request,
        extra_env,
    )?;
    let argv = login_shell_argv(&env, &provider_argv);
    Ok(CompiledAgentProcess {
        provider_argv,
        provider_program,
        argv,
        env,
    })
}

fn merge_appended_system_text(extra_args: &mut Vec<String>, append_args: Vec<String>) {
    let mut append_args = append_args.into_iter();
    let Some(flag) = append_args.next() else {
        return;
    };
    let Some(text) = append_args.next() else {
        return;
    };
    debug_assert!(append_args.next().is_none());

    let matcher = crate::agents::PresetArgMatcher::TextFlag(vec![flag.clone()]);
    let Some(existing) = matcher.occurrences(extra_args).into_iter().last() else {
        extra_args.extend([flag, text]);
        return;
    };
    let merged = format!("{}\n\n{text}", existing.value);
    if existing.argv_range.len() == 1 {
        extra_args[existing.argv_range.start] = format!("{flag}={merged}");
    } else {
        extra_args[existing.argv_range.start + 1] = merged;
    }
}

/// Compile one process and resolve managed-account applicability from its final inputs.
pub fn compile_managed_agent_process(
    project_root: &Path,
    rtk: crate::config::RtkMode,
    request: &ExecRequest,
    cwd: &Path,
    requested: &crate::agents::ManagedLaunchState,
) -> AgentProcessResult<(CompiledAgentProcess, crate::agents::ManagedLaunchState)> {
    let process = compile_agent_process(project_root, rtk, request, cwd)?;
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
/// execute raw provider argv after the adapter verifies the effective binding.
pub fn compile_agent_process_stage_with_extra_env(
    project_root: &Path,
    rtk: crate::config::RtkMode,
    request: &ExecRequest,
    cwd: &Path,
    rimz_bin: &Path,
    extra_env: &BTreeMap<String, String>,
) -> Result<AgentProcessStage, AgentProcessStageErr> {
    let bound = !matches!(&request.provider_account, ProviderAccountState::Unbound);
    if bound && !matches!(&request.action, ExecAction::Launch { .. }) {
        return Err(AgentProcessStageErr::InvalidProviderBinding);
    }

    let process = compile_agent_process_with_extra_env(project_root, rtk, request, cwd, extra_env)?;
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
    finalize_agent_process_stage(process, request, managed_launch.as_ref(), rimz_bin)
}

fn finalize_agent_process_stage(
    process: CompiledAgentProcess,
    request: &ExecRequest,
    managed_launch: Option<&crate::agents::ManagedLaunchState>,
    rimz_bin: &Path,
) -> Result<AgentProcessStage, AgentProcessStageErr> {
    match &request.provider_account {
        ProviderAccountState::Unbound => Ok(AgentProcessStage::Ready(process)),
        ProviderAccountState::Pending { binding } => {
            let mut finalized = request.clone();
            finalized.provider_account = ProviderAccountState::Finalized {
                binding: binding.clone(),
            };
            let argv = exec_argv(rimz_bin, &finalized)?;
            let argv = login_shell_argv(&process.env, &argv);
            if argv.is_empty() {
                return Err(AgentProcessStageErr::EmptyReentry);
            }
            Ok(AgentProcessStage::LoginShellReentry { process, argv })
        }
        ProviderAccountState::Finalized { binding } => {
            if managed_launch.and_then(crate::agents::ManagedLaunchState::binding) != Some(binding)
            {
                return Err(AgentProcessStageErr::FinalizedProviderMismatch);
            }
            let mut process = process;
            process.argv = std::mem::take(&mut process.provider_argv);
            Ok(AgentProcessStage::Ready(process))
        }
    }
}

fn compose_agent_env(
    mut env: BTreeMap<String, String>,
    adapter: &crate::agents::AgentDefinition,
    rtk: crate::config::RtkMode,
    request: &ExecRequest,
    system_prompt_env: &BTreeMap<String, String>,
) -> AgentProcessResult<BTreeMap<String, String>> {
    for (key, value) in adapter.launch_env() {
        env.insert(key.to_owned(), value.to_owned());
    }
    env.extend(system_prompt_env.clone());
    env.extend(exec_identity_env(request));
    env.insert(
        crate::harness::launch::ENV_RTK.to_owned(),
        rtk.as_str().to_owned(),
    );
    if request.subagent {
        adapter.lockdown_subagent_env(&mut env);
    }
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
pub fn preflight_agent_process(
    project_root: &Path,
    rtk: crate::config::RtkMode,
    request: &ExecRequest,
    cwd: &Path,
) -> AgentProcessResult<()> {
    compile_agent_process(project_root, rtk, request, cwd).map(drop)
}

/// Preflight one fresh provider launch before detailed pane identity exists.
pub fn preflight_agent_kind(
    project_root: &Path,
    rtk: crate::config::RtkMode,
    kind: &str,
    cwd: &Path,
) -> AgentProcessResult<()> {
    preflight_agent_process(
        project_root,
        rtk,
        &ExecRequest::bare_launch(AgentKind::new_unchecked(kind), Vec::new()),
        cwd,
    )
}

pub fn exec_argv(rimz_bin: &Path, request: &ExecRequest) -> Result<Vec<String>, ExecWireErr> {
    let mut encoded = request.clone();
    encoded.identity.params.kind_ordinal = None;
    validate_exec_request(&encoded)?;
    let payload = serde_json::to_string(&encoded).map_err(ExecWireErr::Serialize)?;
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
    argv.extend(["--request".to_owned(), payload]);
    Ok(argv)
}

pub fn decode_exec_request(
    visible_kind: &str,
    visible_worktree_path: Option<&Path>,
    payload: &str,
) -> Result<ExecRequest, ExecWireErr> {
    let request: ExecRequest = serde_json::from_str(payload).map_err(ExecWireErr::Parse)?;
    if request.kind != visible_kind {
        return Err(ExecWireErr::KindMismatch {
            visible: visible_kind.to_owned(),
            payload: request.kind.to_string(),
        });
    }
    if request.worktree_path.as_deref() != visible_worktree_path {
        return Err(ExecWireErr::WorktreeMismatch);
    }
    validate_exec_request(&request)?;
    Ok(request)
}

fn validate_exec_request(request: &ExecRequest) -> Result<(), ExecWireErr> {
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
/// Callers merge trust env, adapter launch env, and rtk around it.
pub fn exec_identity_env(request: &ExecRequest) -> BTreeMap<String, String> {
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
    for field in launch_fields(&request.identity.params) {
        if let (Some(key), Some(value)) = (field.env, field.value.into_env()) {
            env.insert(key.to_owned(), value);
        }
    }
    env
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

/// Wrap an agent command in the user's default shell startup path so shell rc
/// env applies, while RimZ's launch env is re-applied after rc processing.
///
/// Launch env is encoded as `KEY=VALUE` argv entries for `/usr/bin/env`, which
/// makes those values visible to same-user process inspection during startup.
/// The inputs today are trusted project config, adapter pins, and run ids; a
/// future secret-bearing launch source needs a two-stage re-exec channel that
/// does not place assignments in argv.
pub fn login_shell_argv(env: &BTreeMap<String, String>, agent_argv: &[String]) -> Vec<String> {
    login_shell_argv_with(
        user_shell().as_deref(),
        Path::new(ENV_BIN).is_file(),
        env,
        agent_argv,
    )
}

fn login_shell_argv_with(
    shell: Option<&Path>,
    env_bin_available: bool,
    env: &BTreeMap<String, String>,
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
    if let Some(key) = invalid_env_key(env) {
        tracing::debug!(
            key,
            "agent launch shell wrapper disabled: launch env key cannot be represented as an env(1) assignment",
        );
        return agent_argv.to_vec();
    }

    match ShellFamily::from_shell(shell) {
        ShellFamily::Bash => bash_interactive_shell_argv(shell, env, agent_argv),
        ShellFamily::Posix => posix_login_shell_argv(shell, env, agent_argv),
        ShellFamily::Fish => fish_login_shell_argv(shell, env, agent_argv),
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
    agent_argv: &[String],
) -> Vec<String> {
    let mut argv = vec![
        shell.to_string_lossy().into_owned(),
        "-i".to_owned(),
        "-c".to_owned(),
        POSIX_LOGIN_SHELL_SCRIPT.to_owned(),
        POSIX_ARG0.to_owned(),
    ];
    argv.extend(env_assignments(env));
    argv.extend(agent_argv.iter().cloned());
    argv
}

fn posix_login_shell_argv(
    shell: &Path,
    env: &BTreeMap<String, String>,
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
    argv.extend(env_assignments(env));
    argv.extend(agent_argv.iter().cloned());
    argv
}

fn fish_login_shell_argv(
    shell: &Path,
    env: &BTreeMap<String, String>,
    agent_argv: &[String],
) -> Vec<String> {
    let mut argv = vec![
        shell.to_string_lossy().into_owned(),
        "-l".to_owned(),
        "-i".to_owned(),
        "-c".to_owned(),
        FISH_LOGIN_SHELL_SCRIPT.to_owned(),
    ];
    argv.extend(env_assignments(env));
    argv.extend(agent_argv.iter().cloned());
    argv
}

fn env_assignments(env: &BTreeMap<String, String>) -> impl Iterator<Item = String> + '_ {
    env.iter().map(|(key, value)| format!("{key}={value}"))
}

fn valid_env_key(key: &str) -> bool {
    !key.is_empty() && !key.contains('=') && !key.starts_with('-')
}

fn invalid_env_key(env: &BTreeMap<String, String>) -> Option<&str> {
    env.keys()
        .find(|key| !valid_env_key(key))
        .map(String::as_str)
}

/// Return whether `program` resolves in the PATH that an agent launch will
/// see after shell startup files and RimZ's launch env are applied.
pub fn program_resolves_after_shell_rc(
    env: &BTreeMap<String, String>,
    program: &str,
) -> Result<bool, ProgramLookupErr> {
    program_resolves_with(
        user_shell().as_deref(),
        Path::new(ENV_BIN).is_file(),
        env,
        program,
    )
}

fn program_resolves_with(
    shell: Option<&Path>,
    env_bin_available: bool,
    env: &BTreeMap<String, String>,
    program: &str,
) -> Result<bool, ProgramLookupErr> {
    let Some(path) = final_launch_path_with(shell, env_bin_available, env)? else {
        return Ok(false);
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    Ok(which::which_in(program, Some(path), cwd).is_ok())
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
    let argv = login_shell_argv_with(shell, env_bin_available, env, &env_probe_argv());
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
    let deadline = Instant::now() + ENV_PROBE_TIMEOUT;
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
                return Err(ProgramLookupErr::ProbeTimeout(ENV_PROBE_TIMEOUT));
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

fn env_shell() -> Option<PathBuf> {
    std::env::var_os("SHELL")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(unix)]
fn passwd_shell() -> Option<PathBuf> {
    let user = nix::unistd::User::from_uid(nix::unistd::Uid::current())
        .ok()
        .flatten()?;
    Some(user.shell)
}

#[cfg(not(unix))]
fn passwd_shell() -> Option<PathBuf> {
    None
}

fn launchable_shell(shell: &Path) -> bool {
    !is_login_disabled_shell(shell) && shell_exists(shell)
}

fn is_login_disabled_shell(shell: &Path) -> bool {
    shell
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| matches!(name, "false" | "nologin"))
        .unwrap_or(false)
}

fn shell_exists(shell: &Path) -> bool {
    if shell.is_absolute() {
        return shell.is_file();
    }
    which::which(shell).is_ok()
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
