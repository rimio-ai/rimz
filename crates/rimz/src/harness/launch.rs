//! Provider-process compilation, exec-wrapper argv, and login-shell launch policy.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::ids::WorkspaceId;

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
    pub provider_argv: Vec<String>,
    /// Provider executable used by PATH preflight.
    pub provider_program: String,
    /// Final shell-wrapped command.
    pub argv: Vec<String>,
    /// Final child environment, also re-applied after shell startup.
    pub env: BTreeMap<String, String>,
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
            crate::harness::run::ENV_WORKTREE_PATH,
            worktree_path.display()
        ),
        format!("{}={channel}", crate::harness::run::ENV_CHANNEL),
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

/// The identity a `rimz agents exec` pane carries: rendered as `--agent-*`
/// flags (parsed back by the CLI's ExecArgs) and as RIMZ_* env
/// (crate::harness::run::ENV_*) for lifecycle hooks and peer attribution.
#[derive(Clone, Copy, Debug, Default)]
pub struct ExecIdentity<'a> {
    pub name: Option<&'a str>,
    /// Provenance for `name`: true only for a user-chosen `--name`, false for
    /// minted and soft names. Rendered as a hidden CLI flag, not an env var.
    pub name_explicit: bool,
    /// Provisional launch row id; rendered only alongside `name`
    /// (`--launch-id` requires `--agent-name` at the parse side).
    pub launch_id: Option<&'a str>,
    /// Canonical launch parameters. `kind_ordinal` remains display-only and is
    /// deliberately absent from wrapper argv and environment wiring.
    pub params: Option<&'a crate::agents::LaunchParams>,
}

#[derive(Clone, Copy)]
enum LaunchFieldValue<'a> {
    Text(Option<&'a str>),
    Mode(Option<crate::harness::run::PermissionMode>),
    Ordinal(Option<u32>),
}

impl LaunchFieldValue<'_> {
    fn push_argv(self, argv: &mut Vec<String>, flag: &'static str) {
        let value = match self {
            Self::Text(Some(value)) => value.to_owned(),
            Self::Mode(Some(value)) => value.to_string(),
            Self::Ordinal(Some(value)) => value.to_string(),
            Self::Text(None) | Self::Mode(None) | Self::Ordinal(None) => return,
        };
        argv.extend([flag.to_owned(), value]);
    }

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
    flag: &'static str,
    env: Option<&'static str>,
    value: LaunchFieldValue<'a>,
}

fn launch_fields(params: Option<&crate::agents::LaunchParams>) -> [LaunchField<'_>; 10] {
    let text = |field: fn(&crate::agents::LaunchParams) -> Option<&str>| {
        LaunchFieldValue::Text(params.and_then(field))
    };
    [
        LaunchField {
            flag: "--agent-profile",
            env: Some(crate::harness::run::ENV_AGENT_PROFILE),
            value: text(|params| params.profile.as_deref()),
        },
        LaunchField {
            flag: "--agent-mode",
            env: None,
            value: LaunchFieldValue::Mode(params.and_then(|params| params.mode)),
        },
        LaunchField {
            flag: "--agent-role",
            env: Some(crate::harness::run::ENV_AGENT_ROLE),
            value: text(|params| params.role.as_deref()),
        },
        LaunchField {
            flag: "--agent-team",
            env: Some(crate::harness::run::ENV_TEAM),
            value: text(|params| params.team.as_deref()),
        },
        LaunchField {
            flag: "--launch-group",
            env: Some(crate::harness::run::ENV_LAUNCH_GROUP),
            value: text(|params| params.launch_group.as_deref()),
        },
        LaunchField {
            flag: "--launch-ordinal",
            env: Some(crate::harness::run::ENV_LAUNCH_ORDINAL),
            value: LaunchFieldValue::Ordinal(params.and_then(|params| params.launch_ordinal)),
        },
        LaunchField {
            flag: "--agent-channel",
            env: Some(crate::harness::run::ENV_CHANNEL),
            value: text(|params| params.channel.as_deref()),
        },
        LaunchField {
            flag: "--agent-model",
            env: Some(crate::harness::run::ENV_AGENT_MODEL),
            value: text(|params| params.model.as_deref()),
        },
        LaunchField {
            flag: "--agent-effort",
            env: Some(crate::harness::run::ENV_AGENT_EFFORT),
            value: text(|params| params.effort.as_deref()),
        },
        LaunchField {
            flag: "--agent-budget",
            env: Some(crate::harness::run::ENV_AGENT_BUDGET),
            value: text(|params| params.budget.as_deref()),
        },
    ]
}

#[derive(Clone, Copy, Debug)]
pub enum ExecAction<'a> {
    Launch {
        prompt: Option<&'a str>,
        extra_args: &'a [String],
    },
    Resume {
        session_id: &'a str,
        extra_args: &'a [String],
    },
    Fork {
        session_id: &'a str,
        extra_args: &'a [String],
    },
}

#[derive(Clone, Copy, Debug)]
pub struct ExecInvocation<'a> {
    pub kind: &'a str,
    pub action: ExecAction<'a>,
    pub run_id: Option<&'a str>,
    pub worktree_path: Option<&'a Path>,
    pub close_pane_on_exit: bool,
    pub exit_on_run_completion: bool,
    pub identity: ExecIdentity<'a>,
}

/// Compile the selected adapter action and preserve provider trailing argv.
pub fn compile_provider_argv(
    adapter: &dyn crate::agents::AgentAdapter,
    kind: &str,
    action: &ExecAction<'_>,
    cwd: &Path,
) -> AgentProcessResult<Vec<String>> {
    let argv = match *action {
        ExecAction::Launch { prompt, extra_args } => adapter
            .launch_command(extra_args, prompt)
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
            let mut argv = adapter.fork_command(session_id, cwd).ok_or_else(|| {
                AgentProcessCompileErr::NoFork {
                    kind: kind.to_owned(),
                }
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
    invocation: &ExecInvocation<'_>,
    cwd: &Path,
) -> AgentProcessResult<CompiledAgentProcess> {
    let adapter = crate::agents::find_adapter(invocation.kind).ok_or_else(|| {
        AgentProcessCompileErr::UnknownAgent {
            kind: invocation.kind.to_owned(),
        }
    })?;
    let provider_argv = compile_provider_argv(adapter, invocation.kind, &invocation.action, cwd)?;
    let provider_program =
        provider_argv
            .first()
            .cloned()
            .ok_or_else(|| AgentProcessCompileErr::EmptyCommand {
                kind: invocation.kind.to_owned(),
            })?;
    let env = compose_agent_env(
        trusted_agent_env(project_root, invocation.kind)?,
        adapter,
        rtk,
        invocation,
    )?;
    let argv = login_shell_argv(&env, &provider_argv);
    Ok(CompiledAgentProcess {
        provider_argv,
        provider_program,
        argv,
        env,
    })
}

fn compose_agent_env(
    mut env: BTreeMap<String, String>,
    adapter: &dyn crate::agents::AgentAdapter,
    rtk: crate::config::RtkMode,
    invocation: &ExecInvocation<'_>,
) -> AgentProcessResult<BTreeMap<String, String>> {
    for (key, value) in adapter.launch_env() {
        env.insert(key.to_owned(), value.to_owned());
    }
    env.extend(exec_identity_env(invocation));
    env.insert(
        crate::harness::run::ENV_RTK.to_owned(),
        rtk.as_str().to_owned(),
    );
    if let Some(key) = invalid_env_key(&env) {
        return Err(AgentProcessCompileErr::InvalidEnvKey {
            kind: invocation.kind.to_owned(),
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
    invocation: &ExecInvocation<'_>,
    cwd: &Path,
) -> AgentProcessResult<()> {
    compile_agent_process(project_root, rtk, invocation, cwd).map(drop)
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
        &ExecInvocation {
            kind,
            action: ExecAction::Launch {
                prompt: None,
                extra_args: &[],
            },
            run_id: None,
            worktree_path: None,
            close_pane_on_exit: false,
            exit_on_run_completion: false,
            identity: ExecIdentity::default(),
        },
        cwd,
    )
}

pub fn exec_argv(rimz_bin: &Path, inv: &ExecInvocation<'_>) -> Vec<String> {
    let mut argv = vec![
        rimz_bin.to_string_lossy().into_owned(),
        "agents".to_owned(),
        "exec".to_owned(),
        inv.kind.to_owned(),
    ];
    match inv.action {
        ExecAction::Resume { session_id, .. } => {
            argv.extend(["--resume".to_owned(), session_id.to_owned()]);
        }
        ExecAction::Fork { session_id, .. } => {
            argv.extend(["--fork".to_owned(), session_id.to_owned()]);
        }
        ExecAction::Launch { .. } => {}
    }
    if let Some(run_id) = inv.run_id {
        argv.extend(["--run-id".to_owned(), run_id.to_owned()]);
    }
    if let Some(name) = inv.identity.name {
        argv.extend(["--agent-name".to_owned(), name.to_owned()]);
        if inv.identity.name_explicit {
            argv.push("--agent-name-explicit".to_owned());
        }
        if let Some(launch_id) = inv.identity.launch_id {
            argv.extend(["--launch-id".to_owned(), launch_id.to_owned()]);
        }
    }
    for field in launch_fields(inv.identity.params) {
        field.value.push_argv(&mut argv, field.flag);
    }
    if inv.exit_on_run_completion {
        argv.push("--exit-on-run-completion".to_owned());
    }
    if inv.close_pane_on_exit {
        argv.push("--close-pane-on-exit".to_owned());
    }
    if let Some(path) = inv.worktree_path {
        argv.extend([
            "--worktree-path".to_owned(),
            path.to_string_lossy().into_owned(),
        ]);
    }
    if let ExecAction::Launch { prompt, .. } = inv.action
        && let Some(prompt) = prompt.filter(|value| !value.is_empty())
    {
        argv.extend(["--prompt".to_owned(), prompt.to_owned()]);
    }
    let extra_args = match inv.action {
        ExecAction::Launch { extra_args, .. }
        | ExecAction::Resume { extra_args, .. }
        | ExecAction::Fork { extra_args, .. } => extra_args,
    };
    if !extra_args.is_empty() {
        argv.push("--".to_owned());
        argv.extend(extra_args.iter().cloned());
    }
    argv
}

/// The RIMZ_* identity env for one invocation (kind, run id, identity fields).
/// Callers merge trust env, adapter launch env, and rtk around it.
pub fn exec_identity_env(inv: &ExecInvocation<'_>) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert(
        crate::harness::run::ENV_AGENT_KIND.to_owned(),
        inv.kind.to_owned(),
    );
    if let Some(run_id) = inv.run_id {
        env.insert(
            crate::harness::run::ENV_RUN_ID.to_owned(),
            run_id.to_owned(),
        );
    }
    if let Some(name) = inv.identity.name {
        env.insert(
            crate::harness::run::ENV_AGENT_NAME.to_owned(),
            name.to_owned(),
        );
    }
    for field in launch_fields(inv.identity.params) {
        if let (Some(key), Some(value)) = (field.env, field.value.into_env()) {
            env.insert(key.to_owned(), value);
        }
    }
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

pub fn invalid_env_key(env: &BTreeMap<String, String>) -> Option<&str> {
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
