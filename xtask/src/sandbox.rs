//! Disposable host-state and multiplexer roots for tests and manual smoke runs.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

mod join;
mod room;

pub(crate) const ROOM_RECORD: &str = "room.json";

#[derive(Serialize, Deserialize)]
pub(crate) struct RoomRecord {
    pub(crate) mux: String,
    pub(crate) session: String,
    pub(crate) worktree: PathBuf,
    pub(crate) repo: PathBuf,
    pub(crate) stub_dir: PathBuf,
    pub(crate) binary: PathBuf,
}

#[derive(Deserialize)]
struct Panes {
    session: String,
    tabs: Vec<Tab>,
}

#[derive(Deserialize)]
struct Tab {
    view_id: Option<String>,
    name: Option<String>,
    panes: Vec<Pane>,
}

#[derive(Deserialize)]
struct Pane {
    pane_id: String,
    kind: String,
    agent: Option<Agent>,
    pid: Option<u32>,
}

#[derive(Deserialize)]
struct Agent {
    handle: String,
}

impl RoomRecord {
    pub(crate) fn read(root: &Path) -> Result<Self> {
        let path = root.join(ROOM_RECORD);
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_slice(&bytes).with_context(|| format!("decoding {}", path.display()))
    }

    pub(crate) fn write(root: &Path, record: &Self) -> Result<()> {
        let path = root.join(ROOM_RECORD);
        std::fs::write(&path, serde_json::to_vec_pretty(record)?)
            .with_context(|| format!("writing {}", path.display()))
    }
}

const CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
const REAPER_WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const SANDBOX_PREFIX: &str = "rimz-sandbox-";
const REAPER_ARG: &str = "__sandbox-reaper";

/// One short-lived filesystem namespace for host-facing test processes.
///
/// XDG roots keep Zellij and RimZ state off the host, while `TMUX_TMPDIR`
/// keeps even a forgotten default `TmuxBackend` away from the user's server.
/// Every sandbox also replaces `HOME`, covering agent configs whose upstream
/// location does not follow XDG. The host's Rust toolchain homes are pinned
/// first, so a nested `cargo` whose launcher resolves rustup through `HOME`
/// still finds the host toolchain rather than the empty sandbox home.
///
/// An independent keepalive reaper applies the same cleanup when the owner dies before `Drop`.
/// It runs in its own process group, so a group signal ending the owner leaves it to clean.
pub(crate) struct HostSandbox {
    _root: TempDir,
    env: BTreeMap<&'static str, PathBuf>,
    toolchain: Vec<(&'static str, PathBuf)>,
    reaper: Option<SandboxReaper>,
}

impl HostSandbox {
    pub(crate) fn for_tests(workspace_root: &Path) -> Result<Self> {
        let mut sandbox = Self::new()?;
        sandbox.trust_workspace_for_git(workspace_root)?;
        let skip_log = sandbox._root.path().join("skipped-tests");
        sandbox.env.insert(SKIP_LOG_ENV, skip_log);
        let missing_codex = sandbox._root.path().join("missing-codex");
        sandbox.env.insert(CODEX_BIN_ENV, missing_codex);
        Ok(sandbox)
    }

    /// The operator's report of the tests that self-skipped during this run,
    /// read from the records the integration harness appended under
    /// [`SKIP_LOG_ENV`]; `None` when no test skipped.
    pub(crate) fn skip_report(&self) -> Option<String> {
        let records = std::fs::read_to_string(self.env.get(SKIP_LOG_ENV)?).ok()?;
        skip_report(&records)
    }

    /// The `--deny-skips` refusal for this run: the self-skipped tests whose
    /// reason `allowed` does not list, then the fix. `None` when every skip is
    /// allowed.
    pub(crate) fn denied_skips(&self, allowed: &AllowedSkips) -> Option<String> {
        let records = std::fs::read_to_string(self.env.get(SKIP_LOG_ENV)?).ok()?;
        denied_skips(&records, allowed)
    }

    fn for_manual_command() -> Result<Self> {
        Self::new()
    }

    fn new() -> Result<Self> {
        let root = tempfile::Builder::new()
            .prefix(SANDBOX_PREFIX)
            .tempdir_in("/tmp")
            .context("creating short test sandbox")?;
        let env = sandbox_env(root.path());
        for path in env.values() {
            std::fs::create_dir_all(path)
                .with_context(|| format!("creating sandbox directory {}", path.display()))?;
        }
        std::fs::write(
            env["ZELLIJ_CONFIG_DIR"].join("config.kdl"),
            "show_startup_tips false\nshow_release_notes false\n",
        )
        .context("writing sandbox Zellij config")?;
        std::fs::write(env["HOME"].join(".zshrc"), "").context("writing sandbox zsh config")?;
        let reaper = SandboxReaper::spawn(root.path())?;
        let toolchain = toolchain_env(|key| std::env::var_os(key));
        Ok(Self {
            _root: root,
            env,
            toolchain,
            reaper,
        })
    }

    fn trust_workspace_for_git(&self, workspace_root: &Path) -> Result<()> {
        let config = self.env["HOME"].join(".gitconfig");
        let output = Command::new("git")
            .arg("config")
            .arg("--file")
            .arg(&config)
            .args(["--add", "safe.directory"])
            .arg(workspace_root)
            .output()
            .context("creating sandbox Git configuration")?;
        if !output.status.success() {
            bail!(
                "creating sandbox Git configuration failed with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    pub(crate) fn command_env(&self) -> Vec<(&'static str, PathBuf)> {
        self.toolchain
            .iter()
            .cloned()
            .chain(self.env.iter().map(|(key, value)| (*key, value.clone())))
            .collect()
    }

    /// Keys a test run drops from the inherited environment: `NO_COLOR`, plus
    /// every ambient room session key (see [`session_key`]). Without this, a
    /// suite launched from inside a RimZ pane or supervised run hands its
    /// `RIMZ_RUN_ID` and identity pin to in-process unit tests, which then take
    /// the supervised branch a clean shell never sees. Keys the sandbox sets
    /// itself, build-script inputs, and park-budget overrides survive: the runner applies removals after
    /// the sandbox env, and dropping a build input would change the binary.
    pub(crate) fn removed_test_env(&self) -> Vec<String> {
        test_removed_env(std::env::vars_os().map(|(key, _)| key), &self.env)
    }

    fn apply_to(&self, command: &mut Command, scrub_session: bool) {
        command.envs(self.toolchain.iter().map(|(key, value)| (key, value)));
        apply_env(command, &self.env, scrub_session);
    }

    #[cfg(test)]
    fn root(&self) -> &Path {
        self._root.path()
    }
}

/// The file a test sandbox names for skip records, one
/// `<test name>\t<reason>\n` line per self-skip, appended by the integration
/// harness's skip helper (`crates/rimz/tests/integration/common/skip.rs`).
const SKIP_LOG_ENV: &str = "RIMZ_TEST_SKIP_LOG";

/// The codex binary override a test sandbox points at a path it never creates,
/// so a test process and every session it births in-process read codex as not
/// installed. The integration scrub sets the same default on each command it
/// builds (`crates/rimz/tests/integration/common/command.rs`); this export
/// covers the births that build no command.
const CODEX_BIN_ENV: &str = "RIMZ_CODEX_BIN";

const SKIP_NAMES_SHOWN: usize = 5;

/// Each distinct test with its reason; a test nextest retried appends its
/// record again and still counts once.
fn skipped_tests(records: &str) -> BTreeMap<&str, &str> {
    records
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .collect()
}

fn skip_report(records: &str) -> Option<String> {
    let tests = skipped_tests(records);
    if tests.is_empty() {
        return None;
    }
    let plural = if tests.len() == 1 { "" } else { "s" };
    Some(format!(
        "self-skipped {} test{plural}:{}",
        tests.len(),
        reason_lines(tests, SKIP_NAMES_SHOWN)
    ))
}

/// One line per reason: its test count, then up to `names_shown` test names.
/// `skips` yields `(test, reason)` pairs in test order.
fn reason_lines<'a>(
    skips: impl IntoIterator<Item = (&'a str, &'a str)>,
    names_shown: usize,
) -> String {
    let mut by_reason: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (name, reason) in skips {
        by_reason.entry(reason).or_default().push(name);
    }
    let mut lines = String::new();
    for (reason, names) in by_reason {
        let shown = names[..names.len().min(names_shown)].join(", ");
        let more = match names.len().saturating_sub(names_shown) {
            0 => String::new(),
            more => format!(", +{more} more"),
        };
        lines.push_str(&format!("\n  {reason} ({}): {shown}{more}", names.len()));
    }
    lines
}

/// The allow-list a `cargo xtask test --deny-skips` run reads, relative to the
/// workspace root.
const ALLOWED_SKIPS_FILE: &str = ".config/allowed-test-skips.txt";

/// The self-skip reasons a `--deny-skips` run tolerates: one entry per line of
/// [`ALLOWED_SKIPS_FILE`], matching every recorded reason that starts with it.
pub(crate) struct AllowedSkips(Vec<String>);

impl AllowedSkips {
    pub(crate) fn load(workspace_root: &Path) -> Result<Self> {
        let path = workspace_root.join(ALLOWED_SKIPS_FILE);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading the --deny-skips allow-list {}", path.display()))?;
        Ok(Self::parse(&text))
    }

    fn parse(text: &str) -> Self {
        Self(
            text.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .map(str::to_owned)
                .collect(),
        )
    }

    fn allows(&self, reason: &str) -> bool {
        self.0.iter().any(|entry| reason.starts_with(entry))
    }
}

/// Every offending test is named: the refusal is what CI acts on. Each
/// recorded reason is judged on its own, since a test can record several (a
/// partial skip, a retry that skips differently) and an allowed one must not
/// stand in for an unlisted one.
fn denied_skips(records: &str, allowed: &AllowedSkips) -> Option<String> {
    let denied: BTreeSet<(&str, &str)> = records
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .filter(|(_, reason)| !allowed.allows(reason))
        .collect();
    let tests = denied.iter().map(|(name, _)| name).collect::<BTreeSet<_>>();
    if tests.is_empty() {
        return None;
    }
    let plural = if tests.len() == 1 { "" } else { "s" };
    Some(format!(
        "--deny-skips: {} test{plural} self-skipped for a reason {ALLOWED_SKIPS_FILE} does not list:{}\n\
         install the missing capability in ci/Dockerfile, or add the reason to {ALLOWED_SKIPS_FILE}",
        tests.len(),
        reason_lines(denied.iter().copied(), usize::MAX),
    ))
}

fn sandbox_env(root: &Path) -> BTreeMap<&'static str, PathBuf> {
    BTreeMap::from([
        ("HOME", root.join("home")),
        ("RIMZ_HOME", root.join("home").join(".rimz")),
        ("TMUX_TMPDIR", root.join("tmux")),
        ("XDG_CACHE_HOME", root.join("cache")),
        ("XDG_CONFIG_HOME", root.join("config")),
        ("XDG_DATA_HOME", root.join("data")),
        ("XDG_RUNTIME_DIR", root.join("runtime")),
        ("XDG_STATE_HOME", root.join("state")),
        ("TMPDIR", root.join("tmp")),
        ("ZELLIJ_CONFIG_DIR", root.join("config").join("zellij")),
    ])
}

/// The host's Rust toolchain homes, resolved before the sandbox replaces
/// `HOME`: an unset `CARGO_HOME` or `RUSTUP_HOME` takes rustup's default
/// under the host `HOME`. A key already set passes through inherited, so it
/// is not repeated here; with no host `HOME` there is nothing to resolve.
fn toolchain_env(
    host: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Vec<(&'static str, PathBuf)> {
    let set = |key: &str| host(key).is_some_and(|value| !value.is_empty());
    let Some(home) = host("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
    else {
        return Vec::new();
    };
    [("CARGO_HOME", ".cargo"), ("RUSTUP_HOME", ".rustup")]
        .into_iter()
        .filter(|(key, _)| !set(key))
        .map(|(key, dir)| (key, home.join(dir)))
        .collect()
}

impl Drop for HostSandbox {
    fn drop(&mut self) {
        if self.reaper.take().is_some_and(SandboxReaper::finish) {
            return;
        }
        cleanup_sandbox(self._root.path(), &self.env);
    }
}

struct SandboxReaper {
    child: Child,
    keepalive: ChildStdin,
}

impl SandboxReaper {
    #[cfg(not(test))]
    fn spawn(root: &Path) -> Result<Option<Self>> {
        let mut command = Command::new(self_executable()?);
        command.arg(REAPER_ARG).arg(root);
        Self::start(&mut command).map(Some)
    }

    #[cfg_attr(
        all(test, not(target_os = "linux")),
        expect(dead_code, reason = "the real reaper role test relies on Linux /proc")
    )]
    fn start(command: &mut Command) -> Result<Self> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .context("starting sandbox cleanup reaper")?;
        let keepalive = child
            .stdin
            .take()
            .context("sandbox cleanup reaper has no keepalive pipe")?;
        Ok(Self { child, keepalive })
    }

    #[cfg(test)]
    fn spawn(root: &Path) -> Result<Option<Self>> {
        validate_sandbox_root(root)?;
        Ok(None)
    }

    fn finish(self) -> bool {
        let Self {
            mut child,
            keepalive,
        } = self;
        drop(keepalive);
        let deadline = Instant::now() + REAPER_WAIT_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return status.success(),
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(None) | Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
            }
        }
    }
}

/// The running xtask image, for a helper that re-enters it at a hidden argument.
pub(crate) fn self_executable() -> Result<PathBuf> {
    // A build that relinks xtask while this process runs unlinks the path `current_exe`
    // reads back (`/proc/self/exe` then names `… (deleted)`), and spawning that path fails
    // with ENOENT. On Linux the magic link itself still execs the running image.
    if cfg!(target_os = "linux") {
        return Ok(PathBuf::from("/proc/self/exe"));
    }
    std::env::current_exe().context("resolving the xtask executable")
}

pub(crate) fn run_reaper_mode(args: &[String]) -> Result<bool> {
    if args.first().is_none_or(|arg| arg != REAPER_ARG) {
        return Ok(false);
    }
    let [_, root] = args else {
        bail!("{REAPER_ARG} requires exactly one sandbox root");
    };
    reap_on_eof(Path::new(root))?;
    Ok(true)
}

fn reap_on_eof(root: &Path) -> Result<()> {
    validate_sandbox_root(root)?;
    let mut buffer = [0_u8; 64];
    let mut input = std::io::stdin().lock();
    while input
        .read(&mut buffer)
        .context("reading reaper keepalive")?
        != 0
    {}
    cleanup_sandbox(root, &sandbox_env(root));
    Ok(())
}

fn validate_sandbox_root(root: &Path) -> Result<()> {
    let suffix = root
        .parent()
        .filter(|parent| *parent == Path::new("/tmp"))
        .and_then(|_| root.file_name())
        .and_then(OsStr::to_str)
        .and_then(|name| name.strip_prefix(SANDBOX_PREFIX))
        .filter(|suffix| {
            suffix.len() == 6 && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
        });
    if suffix.is_none() {
        bail!(
            "sandbox reaper refuses root outside /tmp/{SANDBOX_PREFIX}<six alphanumeric characters>"
        );
    }
    Ok(())
}

fn apply_env(command: &mut Command, env: &BTreeMap<&'static str, PathBuf>, scrub_session: bool) {
    if scrub_session {
        for (key, _) in std::env::vars_os() {
            if session_key(&key) {
                command.env_remove(key);
            }
        }
    }
    command.envs(env);
}

fn cleanup_sandbox(root: &Path, env: &BTreeMap<&'static str, PathBuf>) {
    let tmux_started =
        std::fs::read_dir(&env["TMUX_TMPDIR"]).is_ok_and(|mut entries| entries.next().is_some());
    if tmux_started {
        let mut command = Command::new("tmux");
        apply_env(&mut command, env, true);
        command.arg("kill-server");
        reap_bounded(command);
    }

    if env["XDG_RUNTIME_DIR"].join("zellij").exists() {
        let mut command = Command::new("zellij");
        apply_env(&mut command, env, true);
        command.args(["kill-all-sessions", "--yes"]);
        reap_bounded(command);
    }

    reap_sandbox_processes(root);
    remove_tree_bounded(root);
}

/// Provider home and state locations the adapters resolve ahead of `HOME`. An
/// agent on a named account runs with, say, `CODEX_HOME` pointing at that
/// account, so a sandboxed command that inherited it would install hooks in and
/// start agents against the real account rather than the sandbox `HOME`. This
/// mirrors the integration harness's scrub (each adapter's
/// `config_home_env_keys` and `shared_database_home_env_key` plus
/// `PROVIDER_SIDECAR_KEYS` in
/// `crates/rimz/tests/integration/common/command.rs`); xtask does not link the
/// `rimz` crate, so a new adapter home key is added here by hand.
const PROVIDER_HOME_ENV: [&str; 18] = [
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "COPILOT_HOME",
    "CURSOR_CONFIG_DIR",
    "GROK_HOME",
    "KIMI_CODE_HOME",
    "KIRO_HOME",
    "PI_CODING_AGENT_DIR",
    "QWEN_HOME",
    "AMP_DATA_DIR",
    "CODEX_SQLITE_HOME",
    "COPILOT_OTEL_FILE_EXPORTER_PATH",
    "GROK_AUTH_PATH",
    "PI_AGENT_DIR",
    "PI_CODING_AGENT_SESSION_DIR",
    "QWEN_CODE_SYSTEM_DEFAULTS_PATH",
    "QWEN_CODE_SYSTEM_SETTINGS_PATH",
    "QWEN_RUNTIME_DIR",
];

fn session_key(key: &OsStr) -> bool {
    let key = key.to_string_lossy();
    key.starts_with("RIMZ_")
        || key.starts_with("TMUX")
        || key.starts_with("ZELLIJ")
        || PROVIDER_HOME_ENV.contains(&key.as_ref())
}

/// `crates/rimz/build.rs` inputs that share the `RIMZ_` prefix with room
/// session keys but configure the build rather than the session.
const BUILD_INPUT_ENV: [&str; 4] = [
    "RIMZ_PRICING_JSON_PATH",
    "RIMZ_EMBED_PRESENCE_PLUGIN",
    "RIMZ_BUILD_PROFILE_OVERRIDE",
    "RIMZ_BUILD_VERSION_OVERRIDE",
];

fn test_removed_env(
    ambient: impl Iterator<Item = std::ffi::OsString>,
    sandbox: &BTreeMap<&'static str, PathBuf>,
) -> Vec<String> {
    let mut removed = vec!["NO_COLOR".to_owned()];
    removed.extend(
        ambient
            .filter(|key| session_key(key))
            .filter_map(|key| key.into_string().ok())
            .filter(|key| {
                !sandbox.contains_key(key.as_str())
                    && !BUILD_INPUT_ENV.contains(&key.as_str())
                    && key != "RIMZ_TEST_PARK_BUDGET_MIB"
            }),
    );
    removed
}

fn reap_bounded(mut command: Command) {
    let Ok(mut child) = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    let deadline = Instant::now() + CLEANUP_TIMEOUT;
    loop {
        if child.try_wait().is_ok_and(|status| status.is_some()) {
            return;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn reap_sandbox_processes(root: &Path) {
    let pids = sandbox_processes(root);
    if pids.is_empty() {
        return;
    }
    signal_processes("-TERM", &pids);
    let deadline = Instant::now() + CLEANUP_TIMEOUT;
    loop {
        let remaining = sandbox_processes(root);
        if remaining.is_empty() {
            return;
        }
        if Instant::now() >= deadline {
            signal_processes("-KILL", &remaining);
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(not(target_os = "linux"))]
fn reap_sandbox_processes(_root: &Path) {
    // Explicit tmux and Zellij cleanup remains cross-platform; sweeping an
    // arbitrary leaked child by its environment relies on Linux `/proc`.
}

#[cfg(target_os = "linux")]
fn sandbox_processes(root: &Path) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<u32>().ok())
        .filter(|pid| *pid != std::process::id())
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/environ"))
                .is_ok_and(|environment| environment_mentions_root(&environment, root))
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn environment_mentions_root(environment: &[u8], root: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;

    let root = root.as_os_str().as_bytes();
    environment.split(|byte| *byte == 0).any(|entry| {
        let Some(separator) = entry.iter().position(|byte| *byte == b'=') else {
            return false;
        };
        let value = &entry[separator + 1..];
        value == root
            || value
                .strip_prefix(root)
                .is_some_and(|suffix| suffix.first() == Some(&b'/'))
    })
}

#[cfg(target_os = "linux")]
fn signal_processes(signal: &str, pids: &[u32]) {
    let mut command = Command::new("kill");
    command.arg(signal).arg("--");
    command.args(pids.iter().map(u32::to_string));
    reap_bounded(command);
}

fn remove_tree_bounded(root: &Path) {
    let deadline = Instant::now() + CLEANUP_TIMEOUT;
    loop {
        match std::fs::remove_dir_all(root) {
            Ok(()) => return,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) if Instant::now() >= deadline => return,
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
    }
}

const DEV_BINARY_MISSING: &str =
    "development rimz missing; run cargo build -p rimz --bin rimz --features testkit";

/// Where `cargo build` puts this checkout's development `rimz`.
fn build_dir(workspace: &Path) -> PathBuf {
    crate::files::target_dir(workspace).join("debug")
}

/// Run an arbitrary contributor command with disposable HOME, XDG, tmux, and
/// Zellij roots. The command inherits terminal I/O and runs from the workspace
/// root, with the checkout's build directory first on `PATH`, so a bare `rimz`
/// is this checkout's build rather than the installed release.
pub(crate) fn run(root: &Path, args: &[String]) -> Result<()> {
    let args = match args.first().map(String::as_str) {
        Some("in") => return join::run(root, &args[1..]),
        Some("room") => return room::run(root, &args[1..]),
        Some("--") => &args[1..],
        _ => args,
    };
    let mut command = manual_command(root, &build_dir(root), args)?;
    let sandbox = HostSandbox::for_manual_command()?;
    sandbox.apply_to(&mut command, true);
    let program = command.get_program().to_string_lossy().into_owned();
    let status = command
        .status()
        .with_context(|| format!("running sandboxed command `{program}`"))?;
    if !status.success() {
        bail!("sandboxed command `{program}` exited with {status}");
    }
    Ok(())
}

fn manual_command(root: &Path, build_dir: &Path, args: &[String]) -> Result<Command> {
    let Some((program, program_args)) = args.split_first() else {
        bail!(
            "sandbox requires a command; for example: cargo xtask sandbox -- rimz --zellij doctor"
        );
    };
    if program == "rimz" && !build_dir.join("rimz").is_file() {
        bail!(
            "{DEV_BINARY_MISSING}; `rimz` in the sandbox resolves to {}",
            build_dir.join("rimz").display()
        );
    }
    let program = Path::new(program);
    let program = if program.is_relative() && program.components().count() > 1 {
        root.join(program)
    } else {
        program.to_path_buf()
    };
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(build_dir.to_path_buf()).chain(std::env::split_paths(&inherited)),
    )
    .context("prepending the build directory to PATH")?;
    let mut command = Command::new(program);
    command
        .args(program_args)
        .current_dir(root)
        .env("PATH", path);
    Ok(command)
}

#[cfg(test)]
mod tests;
