//! Agent exec-wrapper and shell launch helpers.

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
    pub profile: Option<&'a str>,
    pub mode: Option<crate::harness::run::PermissionMode>,
    pub role: Option<&'a str>,
    pub team: Option<&'a str>,
    pub launch_group: Option<&'a str>,
    pub launch_ordinal: Option<u32>,
    pub channel: Option<&'a str>,
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
    pub budget: Option<&'a str>,
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
        ExecAction::Fork { session_id } => {
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
    if let Some(profile) = inv.identity.profile {
        argv.extend(["--agent-profile".to_owned(), profile.to_owned()]);
    }
    if let Some(mode) = inv.identity.mode {
        argv.extend(["--agent-mode".to_owned(), mode.to_string()]);
    }
    if let Some(role) = inv.identity.role {
        argv.extend(["--agent-role".to_owned(), role.to_owned()]);
    }
    if let Some(team) = inv.identity.team {
        argv.extend(["--agent-team".to_owned(), team.to_owned()]);
    }
    if let Some(launch_group) = inv.identity.launch_group {
        argv.extend(["--launch-group".to_owned(), launch_group.to_owned()]);
    }
    if let Some(launch_ordinal) = inv.identity.launch_ordinal {
        argv.extend(["--launch-ordinal".to_owned(), launch_ordinal.to_string()]);
    }
    if let Some(channel) = inv.identity.channel {
        argv.extend(["--agent-channel".to_owned(), channel.to_owned()]);
    }
    if let Some(model) = inv.identity.model {
        argv.extend(["--agent-model".to_owned(), model.to_owned()]);
    }
    if let Some(effort) = inv.identity.effort {
        argv.extend(["--agent-effort".to_owned(), effort.to_owned()]);
    }
    if let Some(budget) = inv.identity.budget {
        argv.extend(["--agent-budget".to_owned(), budget.to_owned()]);
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
        ExecAction::Launch { extra_args, .. } | ExecAction::Resume { extra_args, .. } => extra_args,
        ExecAction::Fork { .. } => &[],
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
    if let Some(profile) = inv.identity.profile {
        env.insert(
            crate::harness::run::ENV_AGENT_PROFILE.to_owned(),
            profile.to_owned(),
        );
    }
    if let Some(role) = inv.identity.role {
        env.insert(
            crate::harness::run::ENV_AGENT_ROLE.to_owned(),
            role.to_owned(),
        );
    }
    if let Some(team) = inv.identity.team {
        env.insert(crate::harness::run::ENV_TEAM.to_owned(), team.to_owned());
    }
    if let Some(launch_group) = inv.identity.launch_group {
        env.insert(
            crate::harness::run::ENV_LAUNCH_GROUP.to_owned(),
            launch_group.to_owned(),
        );
    }
    if let Some(launch_ordinal) = inv.identity.launch_ordinal {
        env.insert(
            crate::harness::run::ENV_LAUNCH_ORDINAL.to_owned(),
            launch_ordinal.to_string(),
        );
    }
    if let Some(channel) = inv.identity.channel {
        env.insert(
            crate::harness::run::ENV_CHANNEL.to_owned(),
            channel.to_owned(),
        );
    }
    if let Some(model) = inv.identity.model {
        env.insert(
            crate::harness::run::ENV_AGENT_MODEL.to_owned(),
            model.to_owned(),
        );
    }
    if let Some(effort) = inv.identity.effort {
        env.insert(
            crate::harness::run::ENV_AGENT_EFFORT.to_owned(),
            effort.to_owned(),
        );
    }
    if let Some(budget) = inv.identity.budget {
        env.insert(
            crate::harness::run::ENV_AGENT_BUDGET.to_owned(),
            budget.to_owned(),
        );
    }
    env
}

/// Wrap an agent command in the user's default shell startup path so shell rc
/// env applies, while Rimz's launch env is re-applied after rc processing.
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
/// see after shell startup files and Rimz's launch env are applied.
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
mod tests {
    use super::*;

    fn env(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn exec_argv_renders_maximal_launch_identity() {
        let extra_args = argv(&["--dangerously-skip-permissions"]);
        let invocation = ExecInvocation {
            kind: "claude",
            action: ExecAction::Launch {
                prompt: Some("fix it"),
                extra_args: &extra_args,
            },
            run_id: Some("run_123"),
            worktree_path: Some(Path::new("/repo/worktree")),
            close_pane_on_exit: true,
            exit_on_run_completion: true,
            identity: ExecIdentity {
                name: Some("swift-otter"),
                name_explicit: true,
                launch_id: Some("launch_123"),
                profile: Some("planner"),
                mode: Some(crate::harness::run::PermissionMode::Yolo),
                role: Some("coder"),
                team: Some("forge"),
                launch_group: Some("launch_group_1"),
                launch_ordinal: Some(2),
                channel: Some("design"),
                model: Some("opus"),
                effort: Some("high"),
                budget: None,
            },
        };

        assert_eq!(
            exec_argv(Path::new("/bin/rimz"), &invocation),
            argv(&[
                "/bin/rimz",
                "agents",
                "exec",
                "claude",
                "--run-id",
                "run_123",
                "--agent-name",
                "swift-otter",
                "--agent-name-explicit",
                "--launch-id",
                "launch_123",
                "--agent-profile",
                "planner",
                "--agent-mode",
                "yolo",
                "--agent-role",
                "coder",
                "--agent-team",
                "forge",
                "--launch-group",
                "launch_group_1",
                "--launch-ordinal",
                "2",
                "--agent-channel",
                "design",
                "--agent-model",
                "opus",
                "--agent-effort",
                "high",
                "--exit-on-run-completion",
                "--close-pane-on-exit",
                "--worktree-path",
                "/repo/worktree",
                "--prompt",
                "fix it",
                "--",
                "--dangerously-skip-permissions",
            ])
        );
    }

    #[test]
    fn exec_argv_renders_resume() {
        let extra_args = argv(&["--dangerously-skip-permissions"]);
        let invocation = ExecInvocation {
            kind: "claude",
            action: ExecAction::Resume {
                session_id: "session-1",
                extra_args: &extra_args,
            },
            run_id: None,
            worktree_path: None,
            close_pane_on_exit: true,
            exit_on_run_completion: false,
            identity: ExecIdentity {
                name: Some("swift-otter"),
                profile: Some("planner"),
                role: Some("coder"),
                team: Some("forge"),
                launch_group: Some("launch_group_1"),
                launch_ordinal: Some(2),
                channel: Some("design"),
                ..ExecIdentity::default()
            },
        };

        assert_eq!(
            exec_argv(Path::new("/bin/rimz"), &invocation),
            argv(&[
                "/bin/rimz",
                "agents",
                "exec",
                "claude",
                "--resume",
                "session-1",
                "--agent-name",
                "swift-otter",
                "--agent-profile",
                "planner",
                "--agent-role",
                "coder",
                "--agent-team",
                "forge",
                "--launch-group",
                "launch_group_1",
                "--launch-ordinal",
                "2",
                "--agent-channel",
                "design",
                "--close-pane-on-exit",
                "--",
                "--dangerously-skip-permissions",
            ])
        );
    }

    #[test]
    fn exec_argv_renders_fork() {
        let invocation = ExecInvocation {
            kind: "codex",
            action: ExecAction::Fork {
                session_id: "session-1",
            },
            run_id: None,
            worktree_path: None,
            close_pane_on_exit: true,
            exit_on_run_completion: false,
            identity: ExecIdentity {
                name: Some("swift-otter"),
                profile: Some("planner"),
                channel: Some("design"),
                ..ExecIdentity::default()
            },
        };

        assert_eq!(
            exec_argv(Path::new("/bin/rimz"), &invocation),
            argv(&[
                "/bin/rimz",
                "agents",
                "exec",
                "codex",
                "--fork",
                "session-1",
                "--agent-name",
                "swift-otter",
                "--agent-profile",
                "planner",
                "--agent-channel",
                "design",
                "--close-pane-on-exit",
            ])
        );
    }

    #[test]
    fn exec_identity_env_maps_identity_fields() {
        let invocation = ExecInvocation {
            kind: "claude",
            action: ExecAction::Launch {
                prompt: None,
                extra_args: &[],
            },
            run_id: Some("run_123"),
            worktree_path: None,
            close_pane_on_exit: false,
            exit_on_run_completion: false,
            identity: ExecIdentity {
                name: Some("swift-otter"),
                profile: Some("planner"),
                role: Some("coder"),
                team: Some("forge"),
                launch_group: Some("launch_group_1"),
                launch_ordinal: Some(2),
                channel: Some("design"),
                model: Some("opus"),
                effort: Some("high"),
                ..ExecIdentity::default()
            },
        };

        assert_eq!(
            exec_identity_env(&invocation),
            BTreeMap::from([
                (
                    crate::harness::run::ENV_AGENT_KIND.to_owned(),
                    "claude".to_owned()
                ),
                (
                    crate::harness::run::ENV_RUN_ID.to_owned(),
                    "run_123".to_owned()
                ),
                (
                    crate::harness::run::ENV_AGENT_NAME.to_owned(),
                    "swift-otter".to_owned(),
                ),
                (
                    crate::harness::run::ENV_AGENT_PROFILE.to_owned(),
                    "planner".to_owned(),
                ),
                (
                    crate::harness::run::ENV_AGENT_ROLE.to_owned(),
                    "coder".to_owned()
                ),
                (crate::harness::run::ENV_TEAM.to_owned(), "forge".to_owned()),
                (
                    crate::harness::run::ENV_LAUNCH_GROUP.to_owned(),
                    "launch_group_1".to_owned(),
                ),
                (
                    crate::harness::run::ENV_LAUNCH_ORDINAL.to_owned(),
                    "2".to_owned()
                ),
                (
                    crate::harness::run::ENV_CHANNEL.to_owned(),
                    "design".to_owned()
                ),
                (
                    crate::harness::run::ENV_AGENT_MODEL.to_owned(),
                    "opus".to_owned()
                ),
                (
                    crate::harness::run::ENV_AGENT_EFFORT.to_owned(),
                    "high".to_owned()
                ),
            ])
        );
    }

    #[test]
    fn shell_family_matches_known_basenames() {
        assert_eq!(
            ShellFamily::from_shell(Path::new("/bin/bash")),
            ShellFamily::Bash
        );
        assert_eq!(
            ShellFamily::from_shell(Path::new("/usr/bin/zsh")),
            ShellFamily::Posix
        );
        assert_eq!(
            ShellFamily::from_shell(Path::new("/usr/bin/fish")),
            ShellFamily::Fish
        );
        assert_eq!(
            ShellFamily::from_shell(Path::new("/bin/tcsh")),
            ShellFamily::Csh
        );
        assert_eq!(
            ShellFamily::from_shell(Path::new("/opt/bin/custom")),
            ShellFamily::Posix
        );
    }

    #[test]
    fn bash_wrapper_uses_interactive_rc_shape() {
        let wrapped = login_shell_argv_with(
            Some(Path::new("/bin/bash")),
            true,
            &env(&[("AAA", "one")]),
            &argv(&["codex"]),
        );

        assert_eq!(
            wrapped,
            vec![
                "/bin/bash",
                "-i",
                "-c",
                POSIX_LOGIN_SHELL_SCRIPT,
                POSIX_ARG0,
                "AAA=one",
                "codex",
            ]
        );
    }

    #[test]
    fn posix_wrapper_shape_reapplies_env_after_rc() {
        let wrapped = login_shell_argv_with(
            Some(Path::new("/bin/sh")),
            true,
            &env(&[("AAA", "one"), ("BBB", "two")]),
            &argv(&["codex", "prompt with spaces"]),
        );

        assert_eq!(
            wrapped,
            vec![
                "/bin/sh",
                "-l",
                "-i",
                "-c",
                POSIX_LOGIN_SHELL_SCRIPT,
                POSIX_ARG0,
                "AAA=one",
                "BBB=two",
                "codex",
                "prompt with spaces",
            ]
        );
    }

    #[test]
    fn fish_wrapper_uses_argv_without_a_posix_arg0() {
        let wrapped = login_shell_argv_with(
            Some(Path::new("/usr/bin/fish")),
            true,
            &env(&[("AAA", "one")]),
            &argv(&["claude"]),
        );

        assert_eq!(
            wrapped,
            vec![
                "/usr/bin/fish",
                "-l",
                "-i",
                "-c",
                FISH_LOGIN_SHELL_SCRIPT,
                "AAA=one",
                "claude",
            ]
        );
    }

    #[test]
    fn unsupported_or_unavailable_wrapper_falls_back_to_agent_argv() {
        let command = argv(&["codex"]);
        let launch_env = env(&[("AAA", "one")]);

        assert_eq!(
            login_shell_argv_with(None, true, &launch_env, &command),
            command
        );
        assert_eq!(
            login_shell_argv_with(Some(Path::new("/bin/sh")), false, &launch_env, &command),
            command
        );
        assert_eq!(
            login_shell_argv_with(Some(Path::new("/bin/tcsh")), true, &launch_env, &command),
            command
        );
        assert_eq!(
            login_shell_argv_with(
                Some(Path::new("/bin/sh")),
                true,
                &env(&[("BAD=KEY", "one")]),
                &command,
            ),
            command
        );
        assert_eq!(
            login_shell_argv_with(
                Some(Path::new("/bin/sh")),
                true,
                &env(&[("-BAD", "one")]),
                &command,
            ),
            command
        );
    }

    #[test]
    fn invalid_env_key_reports_the_first_unrepresentable_key() {
        assert_eq!(
            invalid_env_key(&env(&[("BAD=KEY", "one")])),
            Some("BAD=KEY")
        );
        assert_eq!(invalid_env_key(&env(&[("", "one")])), Some(""));
        assert_eq!(invalid_env_key(&env(&[("-BAD", "one")])), Some("-BAD"));
        assert_eq!(invalid_env_key(&env(&[("GOOD_KEY", "one")])), None);
    }

    #[test]
    fn program_lookup_uses_path_from_shell_startup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).expect("mkdir bin");
        let agent = bin_dir.join(unique_probe_program());
        std::fs::write(&agent, "#!/bin/sh\nexit 0\n").expect("write agent");
        chmod_executable(&agent);

        let shell = dir.path().join("bash");
        std::fs::write(
            &shell,
            format!(
                "#!/bin/sh\n\
                 export PATH='{}':\"$PATH\"\n\
                 while [ \"$#\" -gt 0 ]; do\n\
                   case \"$1\" in\n\
                     -c)\n\
                       shift\n\
                       script=$1\n\
                       shift\n\
                       exec /bin/sh -c \"$script\" \"$@\"\n\
                       ;;\n\
                     *) shift ;;\n\
                   esac\n\
                 done\n\
                 exit 127\n",
                bin_dir.display()
            ),
        )
        .expect("write shell");
        chmod_executable(&shell);

        assert!(
            program_resolves_with(
                Some(&shell),
                true,
                &BTreeMap::new(),
                agent.file_name().unwrap().to_str().unwrap()
            )
            .expect("lookup")
        );
    }

    #[test]
    fn program_lookup_rejects_invalid_launch_env_keys() {
        let err = program_resolves_with(
            Some(Path::new("/bin/sh")),
            true,
            &env(&[("BAD=KEY", "one")]),
            "codex",
        )
        .expect_err("invalid key");

        assert!(err.to_string().contains("BAD=KEY"));
    }

    #[test]
    fn launchable_shell_rejects_missing_and_disabled_shells() {
        assert!(!launchable_shell(Path::new("/definitely/not/a/shell")));
        assert!(!launchable_shell(Path::new("/usr/sbin/nologin")));
        assert!(!launchable_shell(Path::new("/bin/false")));
    }

    fn unique_probe_program() -> String {
        format!("rimz-agent-probe-{}", std::process::id())
    }

    fn chmod_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;

        let mut perms = std::fs::metadata(path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod");
    }
}
