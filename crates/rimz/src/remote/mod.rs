//! SSH remote attach: the `[user@]host:<session-or-path>` target grammar, the
//! guarded `ssh` command it compiles to, and the autossh-style reconnect
//! policy with reachability-gated retry waits.
//!
//! `rimz remote connect` makes the local rimz a thin SSH launcher and link
//! supervisor: everything room-shaped — workspace resolution, session birth,
//! sidebar, health gate — runs on the remote host's own `rimz`, and the room
//! renders here because `ssh -t` carries and provisions the terminal. This
//! module is pure: it parses targets, builds `CommandSpec`s, and decides
//! reconnects; the cli owns process I/O.

pub mod aliases;
pub mod bandwidth;
pub mod link;
pub mod reachability;
pub mod setup;
pub mod web;

use std::path::Path;
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::ids::MuxName;
use crate::mux::{CLIENT_SIZE_ENV, CommandSpec};

/// Binary override for tests (`tests/fixtures/ssh-trace`), mirroring
/// `RIMZ_ZELLIJ_BIN` — the single chokepoint every ssh invocation resolves
/// through.
pub const SSH_BIN_ENV: &str = "RIMZ_SSH_BIN";

/// Marks an SSH attach started by the local reconnect supervisor's retry
/// loop, so the remote room start uses its unattended posture.
pub const REMOTE_RECONNECT_ENV: &str = "RIMZ_REMOTE_RECONNECT";

/// Stable per-device identity carried to the remote attach so a replacement
/// can retire an orphaned predecessor before entering the multiplexer.
pub const REMOTE_LINEAGE_ENV: &str = "RIMZ_REMOTE_LINEAGE";

/// Binary override for tests, mirroring `RIMZ_SSH_BIN`.
pub const INFOCMP_BIN_ENV: &str = "RIMZ_INFOCMP_BIN";

/// The exit code OpenSSH reserves for its own transport and usage errors —
/// the "link died" signal the reconnect loop watches for.
pub const SSH_TRANSPORT_EXIT: i32 = 255;

/// The exit code the guarded snippet returns when the remote host has no
/// `rimz` on a repaired PATH; the snippet has already printed the install fix.
pub const REMOTE_RIMZ_MISSING_EXIT: i32 = 127;

/// What the part after the `:` names on the remote host.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RemoteSpec {
    /// Contains `/` or starts with `~` — a directory the remote `rimz start`
    /// resolves (and births a room for, if absent). Relative paths resolve
    /// against the remote `$HOME`, scp-style; a leading `~` is normalized to
    /// `$HOME` so it expands remotely despite quoting.
    Path(String),
    /// A bare word — a session name the remote `rimz attach` reattaches.
    Session(String),
}

/// A parsed `[user@]host:<session-or-path>` SSH attach target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteTarget {
    destination: SshDestination,
    spec: RemoteSpec,
}

/// A plain SSH destination with no room/session suffix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshDestination {
    /// `[user@]host` exactly as ssh wants it (IPv6 keeps its brackets).
    destination: String,
    /// The host alone, for human-facing hints.
    host: String,
}

#[derive(Clone, Copy)]
enum DestinationSuffix {
    Forbidden,
    Required,
}

struct ParsedDestination<'a> {
    destination: SshDestination,
    suffix: Option<&'a str>,
}

/// How a remote target string can fail to parse. Every message carries the
/// expected shape and the fix.
#[derive(Debug, thiserror::Error)]
pub enum RemoteTargetError {
    #[error("remote target is empty; expected `[user@]host:<session-or-path>`")]
    Empty,
    #[error(
        "remote target `{0}` is missing the `:<session-or-path>` part; \
         expected `[user@]host:<session-or-path>` — e.g. `dev-box:query-engine` \
         or `dev-box:~/code/query-engine`"
    )]
    MissingColon(String),
    #[error(
        "remote target `{0}` has an empty host; expected `[user@]host:<session-or-path>` \
         — bracket IPv6 hosts: `[::1]:query-engine`"
    )]
    EmptyHost(String),
    #[error(
        "remote target `{0}` has nothing after the `:`; give a session name \
         (`dev-box:query-engine`) or a path (`dev-box:~/code/query-engine`)"
    )]
    EmptyTarget(String),
    #[error(
        "remote target `{0}` has an unclosed `[` for an IPv6 host; \
         write it as `user@[::1]:<session-or-path>`"
    )]
    UnclosedBracket(String),
    #[error(
        "remote target `{0}` points at another user's home (`~user`), which the \
         quoted remote command cannot expand; spell out the absolute path — \
         e.g. `dev-box:/home/alice/code`"
    )]
    TildeUser(String),
}

impl RemoteTarget {
    /// Parse an scp-flavored `[user@]host:<session-or-path>` target.
    pub fn parse(input: &str) -> Result<Self, RemoteTargetError> {
        let parsed = parse_destination(input, DestinationSuffix::Required)?;
        let target = parsed
            .suffix
            .ok_or_else(|| RemoteTargetError::MissingColon(input.to_owned()))?;
        if target.is_empty() {
            return Err(RemoteTargetError::EmptyTarget(input.to_owned()));
        }
        // `~user` would ride the snippet single-quoted — literal, never
        // expanded — so it fails here with the fix instead of resolving a
        // junk path remotely.
        if target.starts_with('~') && target != "~" && !target.starts_with("~/") {
            return Err(RemoteTargetError::TildeUser(input.to_owned()));
        }
        let spec = if target.contains('/') || target.starts_with('~') {
            RemoteSpec::Path(normalize_tilde(target))
        } else {
            RemoteSpec::Session(target.to_owned())
        };
        Ok(Self {
            destination: parsed.destination,
            spec,
        })
    }

    /// The host alone (no user, no brackets), for human-facing hints.
    pub fn host_display(&self) -> &str {
        &self.destination.host
    }

    /// The SSH destination part of this target.
    pub fn ssh_destination(&self) -> &SshDestination {
        &self.destination
    }
}

impl SshDestination {
    /// Parse a colon-less `[user@]host` SSH destination.
    pub fn parse(input: &str) -> Result<Self, RemoteTargetError> {
        Ok(parse_destination(input, DestinationSuffix::Forbidden)?.destination)
    }

    pub fn as_str(&self) -> &str {
        &self.destination
    }

    pub fn host_display(&self) -> &str {
        &self.host
    }
}

fn parse_destination(
    input: &str,
    suffix: DestinationSuffix,
) -> Result<ParsedDestination<'_>, RemoteTargetError> {
    if input.is_empty() {
        return Err(RemoteTargetError::Empty);
    }
    // A bracketed IPv6 host opens the string or follows the `@` that ends the
    // user prefix. In a room target, `@[` after the first `:` belongs to the
    // suffix rather than the host.
    let bracket = if input.starts_with('[') {
        Some(0)
    } else {
        let candidate = match suffix {
            DestinationSuffix::Required => input.find("@["),
            DestinationSuffix::Forbidden => input.rfind("@["),
        }
        .map(|at| at + 1);
        match (candidate, suffix, input.find(':')) {
            (Some(open), DestinationSuffix::Required, Some(colon)) if open > colon => None,
            (candidate, _, _) => candidate,
        }
    };
    if let Some(open) = bracket {
        let rest = &input[open + 1..];
        let close = rest
            .find(']')
            .ok_or_else(|| RemoteTargetError::UnclosedBracket(input.to_owned()))?;
        let tail = &rest[close + 1..];
        let parsed_suffix = match suffix {
            DestinationSuffix::Required => Some(
                tail.strip_prefix(':')
                    .ok_or_else(|| RemoteTargetError::MissingColon(input.to_owned()))?,
            ),
            DestinationSuffix::Forbidden if tail.is_empty() => None,
            DestinationSuffix::Forbidden => {
                return Err(RemoteTargetError::MissingColon(input.to_owned()));
            }
        };
        let host = &rest[..close];
        if host.is_empty() {
            return Err(RemoteTargetError::EmptyHost(input.to_owned()));
        }
        return Ok(ParsedDestination {
            destination: SshDestination {
                destination: format!("{}[{host}]", &input[..open]),
                host: host.to_owned(),
            },
            suffix: parsed_suffix,
        });
    }
    let (destination, parsed_suffix) = match suffix {
        DestinationSuffix::Required => {
            let colon = input
                .find(':')
                .ok_or_else(|| RemoteTargetError::MissingColon(input.to_owned()))?;
            (&input[..colon], Some(&input[colon + 1..]))
        }
        DestinationSuffix::Forbidden if input.contains(':') => {
            return Err(RemoteTargetError::MissingColon(input.to_owned()));
        }
        DestinationSuffix::Forbidden => (input, None),
    };
    // ssh splits user from host at the last `@`; usernames may contain `@`.
    let host = match destination.rfind('@') {
        Some(at) => &destination[at + 1..],
        None => destination,
    };
    if host.is_empty() {
        return Err(RemoteTargetError::EmptyHost(input.to_owned()));
    }
    Ok(ParsedDestination {
        destination: SshDestination {
            destination: destination.to_owned(),
            host: host.to_owned(),
        },
        suffix: parsed_suffix,
    })
}

/// The ssh program, honoring the `RIMZ_SSH_BIN` test-shim override.
pub fn ssh_program() -> String {
    std::env::var(SSH_BIN_ENV).unwrap_or_else(|_| "ssh".to_owned())
}

/// Whether this process was launched by a remote reconnect retry.
pub fn reconnect_marked() -> bool {
    std::env::var_os(REMOTE_RECONNECT_ENV).is_some_and(|value| !value.is_empty())
}

/// The `infocmp` program, honoring the `RIMZ_INFOCMP_BIN` test-shim override.
pub fn infocmp_program() -> String {
    std::env::var(INFOCMP_BIN_ENV).unwrap_or_else(|_| "infocmp".to_owned())
}

/// Invariant inputs for one terminal attach lifecycle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshAttachOptions {
    pub target: RemoteTarget,
    pub lineage: String,
    pub no_resume: bool,
    pub mux: Option<MuxName>,
    pub term: TermPlan,
    pub truecolor: bool,
    pub client_size: Option<(u16, u16)>,
}

/// Derive the stable, non-secret identity one local device uses for one remote
/// room. Length-prefixing keeps the hash projection unambiguous when fields
/// contain separators.
pub fn remote_lineage(target: &RemoteTarget, local_hostname: &str, local_user: &str) -> String {
    let (spec_kind, spec) = match &target.spec {
        RemoteSpec::Path(path) => ("path", path.as_str()),
        RemoteSpec::Session(session) => ("session", session.as_str()),
    };
    let mut hasher = Sha256::new();
    hasher.update(b"rimz.remote-lineage.v1");
    for field in [
        local_hostname,
        local_user,
        target.host_display(),
        spec_kind,
        spec,
    ] {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field.as_bytes());
    }
    hex::encode(&hasher.finalize()[..8])
}

/// Compiles initial and retry SSH attempts without exposing reconnect flags or
/// duplicating the four plain/ControlMaster command variants at the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshAttachPlan {
    options: SshAttachOptions,
}

#[derive(Clone, Copy)]
enum AttemptPhase {
    Initial,
    Retry,
}

enum ControlMode<'a> {
    Plain,
    Master(&'a Path),
}

pub struct SshAttachAttempt<'a> {
    plan: &'a SshAttachPlan,
    phase: AttemptPhase,
}

impl SshAttachPlan {
    pub fn new(options: SshAttachOptions) -> Self {
        Self { options }
    }

    pub fn target(&self) -> &RemoteTarget {
        &self.options.target
    }

    pub fn initial(&self) -> SshAttachAttempt<'_> {
        SshAttachAttempt {
            plan: self,
            phase: AttemptPhase::Initial,
        }
    }

    pub fn retry(&self) -> SshAttachAttempt<'_> {
        SshAttachAttempt {
            plan: self,
            phase: AttemptPhase::Retry,
        }
    }
}

impl SshAttachAttempt<'_> {
    pub fn plain(&self) -> CommandSpec {
        self.compile(ControlMode::Plain)
    }

    pub fn control(&self, path: &Path) -> CommandSpec {
        self.compile(ControlMode::Master(path))
    }

    /// Compile one full SSH invocation. Keepalive options make transport loss
    /// observable; `-t` carries the terminal; `--` fences the destination; the
    /// guarded snippet remains one argument for the remote login shell.
    fn compile(&self, control: ControlMode<'_>) -> CommandSpec {
        let options = &self.plan.options;
        let control_path = match control {
            ControlMode::Plain => None,
            ControlMode::Master(path) => Some(path),
        };
        CommandSpec::new(ssh_program())
            .args([
                "-o",
                "ServerAliveInterval=5",
                "-o",
                "ServerAliveCountMax=3",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "Compression=yes",
            ])
            .args(control_path.into_iter().flat_map(link::control_options))
            .args(["-t", "--"])
            .arg(options.target.ssh_destination().as_str())
            .arg(guarded_snippet(options, self.phase))
    }
}

/// How the remote session resolves the local terminal. `-t` carries the local
/// `$TERM` across; on hosts that lack its terminfo entry the remote tmux/zellij
/// client aborts, so RimZ provisions it before `exec`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TermPlan {
    /// `$TERM` is universally present remotely — emit nothing.
    Keep,
    /// `$TERM` is non-portable and no local `infocmp` source is available.
    Downgrade,
    /// Ship the local terminfo source and `tic` it on the remote.
    Copy { name: String, source: String },
}

impl TermPlan {
    /// The shell that sets `TERM` for the remote `rimz`, terminated with `; `
    /// so it slots in front of `exec` (empty for `Keep`).
    fn remote_setup(&self) -> String {
        match self {
            TermPlan::Keep => String::new(),
            TermPlan::Downgrade => "export TERM=xterm-256color; ".to_owned(),
            TermPlan::Copy { name, source } => format!(
                "export TERM=xterm-256color; \
                 printf '%s\\n' {source} | tic -x - 2>/dev/null && export TERM={name}; ",
                source = sh_quote(source),
                name = sh_quote(name),
            ),
        }
    }
}

/// Names the remote host can be trusted to resolve; everything else needs
/// provisioning.
pub fn term_needs_terminfo_copy(name: &str) -> bool {
    const UNIVERSAL: &[&str] = &[
        "xterm",
        "xterm-color",
        "xterm-16color",
        "xterm-256color",
        "screen",
        "screen-256color",
        "tmux",
        "tmux-256color",
        "vt100",
        "vt102",
        "vt220",
        "ansi",
        "linux",
        "dumb",
    ];
    !UNIVERSAL.contains(&name)
}

/// Decide how the remote session should resolve the local terminal. Pure: the
/// caller supplies the `infocmp` reader because process I/O lives in the cli.
pub fn term_plan_from(
    term: Option<&str>,
    infocmp: impl FnOnce(&str) -> Option<String>,
) -> TermPlan {
    let Some(name) = term.filter(|term| !term.is_empty()) else {
        return TermPlan::Keep;
    };
    if !term_needs_terminfo_copy(name) {
        return TermPlan::Keep;
    }
    match infocmp(name) {
        Some(source) if !source.trim().is_empty() => TermPlan::Copy {
            name: name.to_owned(),
            source,
        },
        _ => TermPlan::Downgrade,
    }
}

/// The single remote shell command: repair the non-login-shell PATH, fail
/// with the install fix when the host has no `rimz`, provision the carried
/// terminal when needed, then exec into the room.
fn guarded_snippet(options: &SshAttachOptions, phase: AttemptPhase) -> String {
    let (verb, arg) = match &options.target.spec {
        RemoteSpec::Path(path) => ("start", quote_remote_path(path)),
        RemoteSpec::Session(name) => ("attach", sh_quote(name)),
    };
    let mut rimz = format!("rimz {verb} --attach");
    if options.no_resume {
        rimz.push_str(" --no-resume");
    }
    if let Some(mux) = options.mux {
        rimz.push_str(&format!(" --mux {mux}"));
    }
    let mut env_setup = String::new();
    env_setup.push_str(&format!(
        "export {REMOTE_LINEAGE_ENV}={}; ",
        sh_quote(&options.lineage)
    ));
    if matches!(phase, AttemptPhase::Retry) {
        env_setup.push_str("export RIMZ_REMOTE_RECONNECT=1; ");
    }
    if options.truecolor {
        env_setup.push_str("export COLORTERM=truecolor; ");
    }
    env_setup.push_str(&client_size_env_setup(options.client_size));
    env_setup.push_str(&options.term.remote_setup());
    remote_exec_snippet(
        options.target.host_display(),
        &env_setup,
        &format!("{rimz} -- {arg}"),
    )
}

pub(crate) fn client_size_env_setup(client_size: Option<(u16, u16)>) -> String {
    client_size.map_or_else(String::new, |(cols, rows)| {
        format!("export {CLIENT_SIZE_ENV}={cols}x{rows}; ")
    })
}

pub(crate) fn remote_exec_snippet(
    host_display: &str,
    env_setup: &str,
    rimz_command: &str,
) -> String {
    let not_found = sh_quote(&format!(
        "rimz not found on {host_display} — run `rimz remote setup <alias-or-host>` locally, or install rimz manually",
    ));
    format!(
        "{}; \
         command -v rimz >/dev/null 2>&1 || {{ echo {not_found} >&2; exit {code}; }}; \
         {env_setup}exec {rimz_command}",
        remote_path_prefix(),
        code = REMOTE_RIMZ_MISSING_EXIT,
    )
}

pub(crate) fn remote_path_prefix() -> &'static str {
    "PATH=\"$HOME/.cargo/bin:$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:$PATH\""
}

/// POSIX single-quote: wrap in `'…'`, escaping each embedded `'` with the
/// classic `'\''` close-reopen. Safe for any string a shell word can hold.
pub(crate) fn sh_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Normalize a leading `~` to `$HOME` so the path expands on the remote even
/// though the snippet quotes it. `~user` homes are rejected at parse time —
/// quoting would keep them literal.
fn normalize_tilde(target: &str) -> String {
    if target == "~" {
        "$HOME".to_owned()
    } else if let Some(rest) = target.strip_prefix("~/") {
        format!("$HOME/{rest}")
    } else {
        target.to_owned()
    }
}

/// Quote a normalized remote path, keeping a leading `$HOME` outside the
/// single quotes (double-quoted) so the remote shell expands it while the
/// tail stays literal.
pub(crate) fn quote_remote_path(path: &str) -> String {
    match path.strip_prefix("$HOME") {
        Some("") => "\"$HOME\"".to_owned(),
        Some(rest) => format!("\"$HOME\"{}", sh_quote(rest)),
        None => sh_quote(path),
    }
}

/// Render a spec as one pasteable shell line, quoting any argv element a
/// shell would split or expand. [`CommandSpec::display_line`] joins with bare
/// spaces, which is fine for mux specs but garbles the snippet argument.
pub fn display_ssh_command(spec: &CommandSpec) -> String {
    let mut out = display_word(&spec.program);
    for arg in &spec.args {
        out.push(' ');
        out.push_str(&display_word(arg));
    }
    out
}

fn display_word(word: &str) -> String {
    // `[`/`]` stay out of the bare set: a bracketed IPv6 destination is a
    // glob pattern to the shell, so it must be quoted to paste safely.
    let bare = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c));
    if bare {
        word.to_owned()
    } else {
        sh_quote(word)
    }
}

/// Reconnect tuning. Defaults follow autossh: a session must confirm its
/// transport or live past the gatetime to count as established, and retries
/// back off exponentially to a ceiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReconnectPolicy {
    /// How long a session must live to count as established when no probe ack
    /// confirms the transport first (autossh's `AUTOSSH_GATETIME`). A
    /// transport failure before any session establishes is an auth/host
    /// problem, not a drop — fatal, never a password-prompt loop.
    pub gatetime: Duration,
    /// First retry delay; doubles per consecutive failed attempt.
    pub backoff_base: Duration,
    /// Backoff ceiling.
    pub backoff_cap: Duration,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            gatetime: Duration::from_secs(30),
            backoff_base: Duration::from_secs(1),
            backoff_cap: Duration::from_secs(30),
        }
    }
}

impl ReconnectPolicy {
    /// Resolve the policy, honoring the hidden test seams
    /// (`RIMZ_REMOTE_GATETIME_MS`, `RIMZ_REMOTE_BACKOFF_MS`,
    /// `RIMZ_REMOTE_BACKOFF_CAP_MS`).
    pub fn from_env() -> Self {
        let mut policy = Self::default();
        if let Some(gatetime) = env_ms("RIMZ_REMOTE_GATETIME_MS") {
            policy.gatetime = gatetime;
        }
        if let Some(base) = env_ms("RIMZ_REMOTE_BACKOFF_MS") {
            policy.backoff_base = base;
        }
        if let Some(cap) = env_ms("RIMZ_REMOTE_BACKOFF_CAP_MS") {
            policy.backoff_cap = cap;
        }
        policy
    }
}

pub(crate) fn env_ms(key: &str) -> Option<Duration> {
    std::env::var(key)
        .ok()?
        .parse::<u64>()
        .ok()
        .map(Duration::from_millis)
}

/// What the supervisor does with a finished ssh session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// ssh exited 0 — the user detached or the remote room closed cleanly.
    CleanExit,
    /// Not worth retrying — auth failure, missing remote rimz, a stuck remote
    /// room, or a deliberate kill. Surface the exit and stop.
    Fatal {
        /// The exit code to report.
        code: i32,
    },
    /// The link dropped on an established session — reconnect after `delay`.
    Retry {
        /// How long to sleep before the next attempt.
        delay: Duration,
    },
}

/// Classify a finished ssh session. Pure: the caller measures `established`
/// (any session confirmed its transport or lived past the gatetime) and counts
/// consecutive failed attempts since the last established session.
pub fn verdict(
    exit_code: Option<i32>,
    established: bool,
    consecutive_failures: u32,
    policy: &ReconnectPolicy,
) -> Verdict {
    match exit_code {
        Some(0) => Verdict::CleanExit,
        Some(SSH_TRANSPORT_EXIT) if established => Verdict::Retry {
            delay: backoff(
                consecutive_failures,
                policy.backoff_base,
                policy.backoff_cap,
            ),
        },
        Some(code) => Verdict::Fatal { code },
        // Signal-death: something killed ssh deliberately; don't fight it.
        None => Verdict::Fatal { code: 1 },
    }
}

pub struct ReconnectState {
    policy: ReconnectPolicy,
    established: bool,
    consecutive_failures: u32,
}

impl ReconnectState {
    pub fn new(policy: ReconnectPolicy) -> Self {
        Self {
            policy,
            established: false,
            consecutive_failures: 0,
        }
    }

    /// Settle one finished ssh session: a session that confirmed its transport
    /// or lived past the gatetime marks the link established and resets the
    /// failure count; a Retry verdict counts one more consecutive failure.
    pub fn settle(&mut self, exit_code: Option<i32>, established: bool) -> Verdict {
        if established {
            self.established = true;
            self.consecutive_failures = 0;
        }
        let verdict = verdict(
            exit_code,
            self.established,
            self.consecutive_failures,
            &self.policy,
        );
        if matches!(verdict, Verdict::Retry { .. }) {
            self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        }
        verdict
    }

    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// Start a fresh backoff ladder after a confirmed network transition.
    pub fn network_restored(&mut self) {
        self.consecutive_failures = 0;
    }

    /// Settle an intentional zombie-transport kill without classifying its
    /// signal exit as fatal.
    pub fn settle_zombie_kill(&mut self) {
        self.established = true;
        self.consecutive_failures = 0;
    }
}

/// Capped exponential backoff shared by ssh reconnects and probe respawns.
pub fn backoff(failures: u32, base: Duration, cap: Duration) -> Duration {
    let factor = 2u32.saturating_pow(failures.min(16));
    base.saturating_mul(factor).min(cap)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
