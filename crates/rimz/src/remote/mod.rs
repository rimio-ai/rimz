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
pub mod forward;
pub mod link;
pub mod reachability;
pub mod recovery;
pub mod setup;
pub mod tty;
pub mod version;
pub mod web;

use std::path::Path;
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::ids::MuxName;
use crate::mux::{CLIENT_SIZE_ENV, CommandSpec};

/// Binary override for tests (`tests/fixtures/ssh-trace`), mirroring
/// `RIMZ_ZELLIJ_BIN` — the single chokepoint every ssh invocation resolves
/// through.
const SSH_BIN_ENV: &str = "RIMZ_SSH_BIN";

/// Marks an SSH attach started by the local reconnect supervisor's retry
/// loop, so the remote room start uses its unattended posture.
pub const REMOTE_RECONNECT_ENV: &str = "RIMZ_REMOTE_RECONNECT";

/// Requests the in-band green attach marker from a compatible remote RimZ.
pub const ATTACH_MARK_ENV: &str = "RIMZ_ATTACH_MARK";

/// Marks a remote room launch whose SSH parent already owns the terminal's
/// alternate-scroll bracket.
pub const OUTER_SCROLL_BRACKET_ENV: &str = "RIMZ_OUTER_SCROLL_BRACKET";

/// Stable per-device identity carried to the remote attach so a replacement
/// can retire an orphaned predecessor before entering the multiplexer.
pub const REMOTE_LINEAGE_ENV: &str = "RIMZ_REMOTE_LINEAGE";

/// Local client version carried to the remote RimZ for launch-time skew
/// notices.
pub const REMOTE_CLIENT_VERSION_ENV: &str = "RIMZ_REMOTE_CLIENT_VERSION";

/// Marks a remote attach that may proceed across a minor version mismatch.
pub const REMOTE_FORCE_VERSION_ENV: &str = "RIMZ_REMOTE_FORCE_VERSION";

/// Binary override for tests, mirroring `RIMZ_SSH_BIN`.
const INFOCMP_BIN_ENV: &str = "RIMZ_INFOCMP_BIN";

/// The exit code OpenSSH reserves for its own transport and usage errors —
/// the "link died" signal the reconnect loop watches for.
pub const SSH_TRANSPORT_EXIT: i32 = 255;

/// The exit code the guarded snippet returns when the remote host has no
/// `rimz` on a repaired PATH; the snippet has already printed the install fix.
pub const REMOTE_RIMZ_MISSING_EXIT: i32 = 127;

/// The remote host refused a bypassable minor version mismatch.
pub const REMOTE_VERSION_SKEW_EXIT: i32 = 65;

/// The remote host refused a hard major version mismatch.
pub const REMOTE_VERSION_INCOMPATIBLE_EXIT: i32 = 66;

/// The guarded snippet refused a remote workspace path that does not exist.
pub const REMOTE_PATH_MISSING_EXIT: i32 = 67;

/// A supervised remote mux client exited successfully, but its session no
/// longer exists.
pub const REMOTE_SESSION_LOST_EXIT: i32 = 68;

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

    /// The remote workspace path, when this target births or enters by path.
    pub fn remote_path(&self) -> Option<&str> {
        match &self.spec {
            RemoteSpec::Path(path) => Some(path),
            RemoteSpec::Session(_) => None,
        }
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
    pub force_version: bool,
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
    mark: bool,
    outer_scroll_bracket: bool,
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
            mark: false,
            outer_scroll_bracket: false,
        }
    }

    pub fn retry(&self) -> SshAttachAttempt<'_> {
        SshAttachAttempt {
            plan: self,
            phase: AttemptPhase::Retry,
            mark: false,
            outer_scroll_bracket: false,
        }
    }

    /// Compile the unattended ControlMaster used to prove transport and auth
    /// behind the recovery panel before the tty attach begins. The lifecycle
    /// and attempt options preserve the supervisor's child-alive iff
    /// master-alive invariant despite inherited SSH configuration.
    pub fn master(&self, control_path: &Path, connect_timeout: Duration) -> CommandSpec {
        let connect_timeout_secs = connect_timeout.as_millis().div_ceil(1000).max(1);
        CommandSpec::new(ssh_program())
            .args([
                "-o",
                "ServerAliveInterval=5",
                "-o",
                "ServerAliveCountMax=3",
                "-o",
            ])
            .arg(format!("ConnectTimeout={connect_timeout_secs}"))
            .args([
                "-o",
                "Compression=yes",
                "-M",
                "-N",
                "-o",
                "BatchMode=yes",
                "-o",
                "ControlPersist=no",
                "-o",
                "ConnectionAttempts=1",
                "-o",
                "ClearAllForwardings=yes",
                "-o",
            ])
            .arg(format!("ControlPath={}", control_path.display()))
            .args(["--", self.options.target.ssh_destination().as_str()])
    }

    /// Probe a path target over an established ControlMaster before the tty
    /// attach. Session targets need no filesystem precondition.
    pub fn path_preflight(&self, control_path: &Path) -> Option<(CommandSpec, &str)> {
        let path = self.options.target.remote_path()?;
        let spec = CommandSpec::new(ssh_program())
            .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"])
            .args(control_options(control_path))
            .args(["--", self.options.target.ssh_destination().as_str()])
            .arg(format!("test -d {}", quote_remote_path(path)));
        Some((spec, path))
    }
}

impl SshAttachAttempt<'_> {
    pub fn with_mark(mut self, mark: bool) -> Self {
        self.mark = mark;
        self
    }

    pub fn with_outer_scroll_bracket(mut self, outer_scroll_bracket: bool) -> Self {
        self.outer_scroll_bracket = outer_scroll_bracket;
        self
    }

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
            .args(control_path.into_iter().flat_map(control_options))
            .args(["-t", "--"])
            .arg(options.target.ssh_destination().as_str())
            .arg(guarded_snippet(
                options,
                self.phase,
                self.mark,
                self.outer_scroll_bracket,
            ))
    }
}

/// Compile the interactive attach's control options. The attach can create the
/// master, so its lifetime stays tied to the supervised child rather than an
/// inherited `ControlPersist` background process holding RimZ's socket.
fn control_options(path: &Path) -> Vec<String> {
    vec![
        "-o".to_owned(),
        "ControlMaster=auto".to_owned(),
        "-o".to_owned(),
        format!("ControlPath={}", path.display()),
        "-o".to_owned(),
        "ControlPersist=no".to_owned(),
    ]
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
fn term_needs_terminfo_copy(name: &str) -> bool {
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
fn guarded_snippet(
    options: &SshAttachOptions,
    phase: AttemptPhase,
    mark: bool,
    outer_scroll_bracket: bool,
) -> String {
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
    env_setup.push_str(&format!(
        "export {REMOTE_CLIENT_VERSION_ENV}={}; ",
        sh_quote(crate::build_id::VERSION)
    ));
    if options.force_version {
        env_setup.push_str(&format!("export {REMOTE_FORCE_VERSION_ENV}=1; "));
    }
    if matches!(phase, AttemptPhase::Retry) {
        env_setup.push_str("export RIMZ_REMOTE_RECONNECT=1; ");
    }
    if mark {
        env_setup.push_str(&format!("export {ATTACH_MARK_ENV}=1; "));
    }
    if outer_scroll_bracket {
        env_setup.push_str(&format!("export {OUTER_SCROLL_BRACKET_ENV}=1; "));
    }
    if options.truecolor {
        env_setup.push_str("export COLORTERM=truecolor; ");
    }
    env_setup.push_str(&client_size_env_setup(options.client_size));
    env_setup.push_str(&options.term.remote_setup());
    remote_exec_snippet(
        options.target.host_display(),
        &env_setup,
        &remote_path_guard(&options.target),
        &format!("{rimz} -- {arg}"),
    )
}

fn remote_path_guard(target: &RemoteTarget) -> String {
    let Some(path) = target.remote_path() else {
        return String::new();
    };
    let arg = quote_remote_path(path);
    let missing = sh_quote(&format!(
        "remote path does not exist on {}: {path}; check the target with `rimz remote list`, then correct the alias or remote path",
        target.host_display()
    ));
    format!("test -d {arg} || {{ echo {missing} >&2; exit {REMOTE_PATH_MISSING_EXIT}; }}; ")
}

fn client_size_env_setup(client_size: Option<(u16, u16)>) -> String {
    client_size.map_or_else(String::new, |(cols, rows)| {
        format!("export {CLIENT_SIZE_ENV}={cols}x{rows}; ")
    })
}

fn remote_exec_snippet(
    host_display: &str,
    env_setup: &str,
    path_guard: &str,
    rimz_command: &str,
) -> String {
    let not_found = sh_quote(&format!(
        "rimz not found on {host_display} — run `rimz remote setup <alias-or-host>` locally, or install rimz manually",
    ));
    format!(
        "{}; \
         command -v rimz >/dev/null 2>&1 || {{ echo {not_found} >&2; exit {code}; }}; \
         {path_guard}{env_setup}exec {rimz_command}",
        remote_path_prefix(),
        code = REMOTE_RIMZ_MISSING_EXIT,
    )
}

fn remote_path_prefix() -> &'static str {
    "PATH=\"$HOME/.cargo/bin:$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:$PATH\""
}

/// POSIX single-quote: wrap in `'…'`, escaping each embedded `'` with the
/// classic `'\''` close-reopen. Safe for any string a shell word can hold.
fn sh_quote(s: &str) -> String {
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
fn quote_remote_path(path: &str) -> String {
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

/// Reconnect tuning. A session must confirm its transport or live past the
/// gatetime to count as established; retry pacing follows network state and
/// outage age.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReconnectPolicy {
    /// How long a session must live to count as established when no probe ack
    /// confirms the transport first (autossh's `AUTOSSH_GATETIME`). A failed
    /// foreground interactive fallback before establishment remains fatal so
    /// RimZ never loops a password prompt.
    pub gatetime: Duration,
    /// Backoff ceiling.
    pub backoff_cap: Duration,
    /// Retry delay while the SSH endpoint remains reachable.
    pub reachable_retry: Duration,
    /// Outage window that keeps reachable-endpoint retries flat and fast.
    pub flat_window: Duration,
    /// Maximum wait for one background master's TCP connect and SSH banner
    /// exchange. The visible attach retains its ten-second connect budget.
    pub master_connect_timeout: Duration,
    /// Maximum lifetime of one background ControlMaster connection attempt.
    pub master_deadline: Duration,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            gatetime: Duration::from_secs(30),
            backoff_cap: Duration::from_secs(30),
            reachable_retry: Duration::from_secs(2),
            flat_window: Duration::from_secs(3 * 60),
            master_connect_timeout: Duration::from_secs(5),
            master_deadline: Duration::from_secs(30),
        }
    }
}

impl ReconnectPolicy {
    /// Resolve the policy, honoring the hidden test seams
    /// (`RIMZ_REMOTE_GATETIME_MS`, `RIMZ_REMOTE_BACKOFF_CAP_MS`,
    /// `RIMZ_REMOTE_REACHABLE_RETRY_MS`, `RIMZ_REMOTE_FLAT_WINDOW_MS`,
    /// `RIMZ_REMOTE_MASTER_CONNECT_MS`, `RIMZ_REMOTE_MASTER_TIMEOUT_MS`).
    pub fn from_env() -> Self {
        let mut policy = Self::default();
        if let Some(gatetime) = env_ms("RIMZ_REMOTE_GATETIME_MS") {
            policy.gatetime = gatetime;
        }
        if let Some(cap) = env_ms("RIMZ_REMOTE_BACKOFF_CAP_MS") {
            policy.backoff_cap = cap;
        }
        if let Some(retry) = env_ms("RIMZ_REMOTE_REACHABLE_RETRY_MS") {
            policy.reachable_retry = retry;
        }
        if let Some(window) = env_ms("RIMZ_REMOTE_FLAT_WINDOW_MS") {
            policy.flat_window = window;
        }
        if let Some(connect_timeout) = env_ms("RIMZ_REMOTE_MASTER_CONNECT_MS") {
            policy.master_connect_timeout = connect_timeout;
        }
        if let Some(deadline) = env_ms("RIMZ_REMOTE_MASTER_TIMEOUT_MS") {
            policy.master_deadline = deadline;
        }
        policy
    }

    /// Pace SSH retries from outage age when direct TCP dials prove the
    /// endpoint remains reachable.
    fn reachable_delay(&self, outage_age: Duration) -> Duration {
        if outage_age < self.flat_window {
            return self.reachable_retry.min(self.backoff_cap);
        }
        let minutes_past_window = outage_age.saturating_sub(self.flat_window).as_secs() / 60;
        let exponent = u32::try_from(minutes_past_window)
            .unwrap_or(u32::MAX)
            .saturating_add(1);
        backoff(exponent, self.reachable_retry, self.backoff_cap)
    }

    /// Pace hidden safety attempts while every configured network checkpoint
    /// is down: 1s through 10s, then 20s, then the 30s ceiling.
    fn unreachable_delay(&self, failures: u32) -> Duration {
        let seconds = match failures {
            0..=9 => u64::from(failures) + 1,
            10 => 20,
            _ => 30,
        };
        Duration::from_secs(seconds).min(self.backoff_cap)
    }
}

/// Whether an OpenSSH error summary describes a transport failure.
///
/// Unknown initial-connect errors fall back to interactive SSH so the user
/// can see and answer the real authentication or host-key prompt.
pub fn transport_failure(summary: &str) -> bool {
    let summary = summary.to_ascii_lowercase();
    [
        "timed out",
        "connection refused",
        "no route to host",
        "network is unreachable",
        "could not resolve hostname",
        "temporary failure in name resolution",
        "connection reset",
    ]
    .iter()
    .any(|needle| summary.contains(needle))
}

/// Extract the useful tail of OpenSSH stderr for the recovery panel.
pub fn ssh_error_summary(stderr: &str) -> Option<String> {
    let line = stderr.lines().rev().find(|line| !line.trim().is_empty())?;
    Some(summarize_ssh_line(line))
}

/// Extract the useful tail of a confirmed ControlMaster attach's stderr.
pub fn attach_error_summary(stderr: &str) -> Option<String> {
    let line = stderr.lines().rev().find(|line| {
        let line = line.trim();
        !line.is_empty() && !is_ssh_close_notice(line)
    })?;
    let lower = line.to_ascii_lowercase();
    if lower.contains("mux_client")
        || lower.contains("read from master failed")
        || lower.contains("control socket connect")
    {
        return Some("SSH control connection dropped".to_owned());
    }
    Some(summarize_ssh_line(line))
}

/// Whether OpenSSH reports that Ctrl-C interrupted the remote command.
///
/// SSH maps a remote `SIGINT` to its generic status 255, so the diagnostic is
/// the only distinction from a dropped established transport.
pub fn attach_interrupted(exit_code: Option<i32>, stderr: &str) -> bool {
    exit_code == Some(SSH_TRANSPORT_EXIT)
        && stderr
            .lines()
            .any(|line| line.trim() == "Killed by signal 2.")
}

fn is_ssh_close_notice(line: &str) -> bool {
    (line.starts_with("Connection to ") || line.starts_with("Shared connection to "))
        && (line.ends_with(" closed.") || line.ends_with(" closed by remote host."))
}

fn summarize_ssh_line(line: &str) -> String {
    use unicode_width::UnicodeWidthChar as _;

    const MAX_CELLS: usize = 80;
    let line = line.trim().strip_prefix("ssh: ").unwrap_or(line.trim());
    let width = line
        .chars()
        .map(|ch| ch.width().unwrap_or(0))
        .sum::<usize>();
    if width <= MAX_CELLS {
        return line.to_owned();
    }
    let mut used = 0;
    let mut summary = line
        .chars()
        .take_while(|ch| {
            let next = used + ch.width().unwrap_or(0);
            if next > MAX_CELLS - 1 {
                return false;
            }
            used = next;
            true
        })
        .collect::<String>();
    summary.push('…');
    summary
}

fn env_ms(key: &str) -> Option<Duration> {
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
    /// A lost remote session is translated to [`REMOTE_SESSION_LOST_EXIT`]
    /// before ssh returns.
    CleanExit,
    /// Not worth retrying — auth failure, missing remote rimz, a stuck remote
    /// room, or a deliberate kill. Surface the exit and stop.
    Fatal {
        /// The exit code to report.
        code: i32,
    },
    /// The link dropped on an established session — enter background recovery.
    Retry,
    /// SSH is still healthy, but an established multiplexer client ended
    /// abnormally — attach a replacement over the existing connection.
    Reattach,
}

#[derive(Default)]
pub struct ReconnectState {
    established: bool,
    consecutive_failures: u32,
}

impl ReconnectState {
    /// Settle one finished ssh process. Terminal attaches supply ControlMaster
    /// liveness and whether the foreground client lived past the gatetime;
    /// callers without a multiplexer client pass `None`.
    pub fn settle(
        &mut self,
        exit_code: Option<i32>,
        established: bool,
        evidence: Option<(bool, bool)>,
    ) -> Verdict {
        if established {
            self.established = true;
            self.consecutive_failures = 0;
        }
        let verdict = match exit_code {
            Some(0) => Verdict::CleanExit,
            Some(
                code @ (REMOTE_RIMZ_MISSING_EXIT
                | REMOTE_VERSION_SKEW_EXIT
                | REMOTE_VERSION_INCOMPATIBLE_EXIT
                | REMOTE_PATH_MISSING_EXIT),
            ) => Verdict::Fatal { code },
            Some(SSH_TRANSPORT_EXIT) if self.established => Verdict::Retry,
            Some(_)
                if self.established
                    && evidence.is_some_and(|(control_alive, _)| !control_alive) =>
            {
                Verdict::Retry
            }
            Some(_)
                if evidence.is_some_and(|(control_alive, lived_past_gatetime)| {
                    control_alive && lived_past_gatetime
                }) =>
            {
                Verdict::Reattach
            }
            Some(code) => Verdict::Fatal { code },
            // Signal-death: something killed ssh deliberately; don't fight it.
            None => Verdict::Fatal { code: 1 },
        };
        if matches!(verdict, Verdict::Retry) {
            self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        }
        verdict
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
