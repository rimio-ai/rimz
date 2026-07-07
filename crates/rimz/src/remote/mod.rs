//! SSH remote attach: the `[user@]host:<session-or-path>` target grammar, the
//! guarded `ssh` command it compiles to, and the autossh-style reconnect
//! policy.
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
pub mod web;

use std::path::Path;
use std::time::Duration;

use crate::ids::MuxName;
use crate::mux::CommandSpec;

/// Binary override for tests (`tests/fixtures/ssh-trace`), mirroring
/// `RIMZ_ZELLIJ_BIN` — the single chokepoint every ssh invocation resolves
/// through.
pub const SSH_BIN_ENV: &str = "RIMZ_SSH_BIN";

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
    /// `[user@]host` exactly as ssh wants it (IPv6 keeps its brackets).
    destination: String,
    /// The host alone, for human-facing hints.
    host: String,
    spec: RemoteSpec,
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
        if input.is_empty() {
            return Err(RemoteTargetError::Empty);
        }
        // A bracketed IPv6 host opens the string or follows the `@` that ends
        // the user prefix (scp's own rule); an `@[` past the first `:` is
        // target content, not a host.
        let bracket = if input.starts_with('[') {
            Some(0)
        } else {
            match (input.find("@["), input.find(':')) {
                (Some(at), Some(colon)) if at < colon => Some(at + 1),
                (Some(at), None) => Some(at + 1),
                _ => None,
            }
        };
        let (user_at, host_raw, host, target) = if let Some(open) = bracket {
            let rest = &input[open + 1..];
            let close = rest
                .find(']')
                .ok_or_else(|| RemoteTargetError::UnclosedBracket(input.to_owned()))?;
            let host = &rest[..close];
            let target = rest[close + 1..]
                .strip_prefix(':')
                .ok_or_else(|| RemoteTargetError::MissingColon(input.to_owned()))?;
            (&input[..open], format!("[{host}]"), host.to_owned(), target)
        } else {
            // The first `:` ends the host; the target keeps any `:` or `@` of
            // its own. ssh splits user from host at the last `@` (usernames
            // can carry their own), so the host hint does too.
            let colon = input
                .find(':')
                .ok_or_else(|| RemoteTargetError::MissingColon(input.to_owned()))?;
            let destination = &input[..colon];
            let (user_at, host) = match destination.rfind('@') {
                Some(at) => destination.split_at(at + 1),
                None => ("", destination),
            };
            (
                user_at,
                host.to_owned(),
                host.to_owned(),
                &input[colon + 1..],
            )
        };
        if host.is_empty() {
            return Err(RemoteTargetError::EmptyHost(input.to_owned()));
        }
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
            destination: format!("{user_at}{host_raw}"),
            host,
            spec,
        })
    }

    /// The host alone (no user, no brackets), for human-facing hints.
    pub fn host_display(&self) -> &str {
        &self.host
    }
}

/// The ssh program, honoring the `RIMZ_SSH_BIN` test-shim override.
pub fn ssh_program() -> String {
    std::env::var(SSH_BIN_ENV).unwrap_or_else(|_| "ssh".to_owned())
}

/// The `infocmp` program, honoring the `RIMZ_INFOCMP_BIN` test-shim override.
pub fn infocmp_program() -> String {
    std::env::var(INFOCMP_BIN_ENV).unwrap_or_else(|_| "infocmp".to_owned())
}

/// Compile a remote target into the full ssh invocation.
///
/// The keepalive options give the reconnect loop its dead-link signal
/// (~15s detection); `-t` forces a remote PTY so the room renders
/// interactively and carries the local `$TERM`, which the snippet provisions
/// when needed; `--` stops ssh option parsing before a destination that could
/// look flag-ish. The final argument is the guarded snippet — one argv element;
/// ssh hands it to the remote login shell as the command string.
pub fn ssh_attach_spec(
    target: &RemoteTarget,
    no_resume: bool,
    mux: Option<MuxName>,
    term: &TermPlan,
    truecolor: bool,
    control: Option<&Path>,
) -> CommandSpec {
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
        .args(control.into_iter().flat_map(link::control_options))
        .args(["-t", "--"])
        .arg(target.destination.clone())
        .arg(guarded_snippet(target, no_resume, mux, term, truecolor))
}

/// How the remote session resolves the local terminal. `-t` carries the local
/// `$TERM` across; on hosts that lack its terminfo entry the remote tmux/zellij
/// client aborts, so Rimz provisions it before `exec`.
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
fn guarded_snippet(
    target: &RemoteTarget,
    no_resume: bool,
    mux: Option<MuxName>,
    term: &TermPlan,
    truecolor: bool,
) -> String {
    let (verb, arg) = match &target.spec {
        RemoteSpec::Path(path) => ("start", quote_remote_path(path)),
        RemoteSpec::Session(name) => ("attach", sh_quote(name)),
    };
    let mut rimz = format!("rimz {verb} --attach");
    if no_resume {
        rimz.push_str(" --no-resume");
    }
    if let Some(mux) = mux {
        rimz.push_str(&format!(" --mux {mux}"));
    }
    let mut env_setup = String::new();
    if truecolor {
        env_setup.push_str("export COLORTERM=truecolor; ");
    }
    env_setup.push_str(&term.remote_setup());
    remote_exec_snippet(&target.host, &env_setup, &format!("{rimz} -- {arg}"))
}

pub(crate) fn remote_exec_snippet(
    host_display: &str,
    env_setup: &str,
    rimz_command: &str,
) -> String {
    let not_found = sh_quote(&format!(
        "rimz not found on {host_display} — install: cargo install --locked rimz",
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

/// Reconnect tuning. Defaults follow autossh: a session must live past the
/// gatetime to count as established, and retries back off exponentially to a
/// ceiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReconnectPolicy {
    /// How long a session must live to count as established (autossh's
    /// `AUTOSSH_GATETIME`). A transport failure before any session
    /// establishes is an auth/host problem, not a drop — fatal, never a
    /// password-prompt loop.
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
    /// (`RIMZ_REMOTE_GATETIME_MS`, `RIMZ_REMOTE_BACKOFF_MS`).
    pub fn from_env() -> Self {
        let mut policy = Self::default();
        if let Some(gatetime) = env_ms("RIMZ_REMOTE_GATETIME_MS") {
            policy.gatetime = gatetime;
        }
        if let Some(base) = env_ms("RIMZ_REMOTE_BACKOFF_MS") {
            policy.backoff_base = base;
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
/// (any session has lived past the gatetime) and counts consecutive failed
/// attempts since the last established session.
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

    /// Settle one finished ssh session: a session that lived past the gatetime
    /// marks the link established and resets the failure count; a Retry verdict
    /// counts one more consecutive failure.
    pub fn settle(&mut self, exit_code: Option<i32>, lived_past_gatetime: bool) -> Verdict {
        if lived_past_gatetime {
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
}

/// Capped exponential backoff shared by ssh reconnects and probe respawns.
pub fn backoff(failures: u32, base: Duration, cap: Duration) -> Duration {
    let factor = 2u32.saturating_pow(failures.min(16));
    base.saturating_mul(factor).min(cap)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> RemoteTarget {
        RemoteTarget::parse(input).expect("target parses")
    }

    #[test]
    fn target_grammar_accepts_supported_forms() {
        struct TargetCase {
            input: &'static str,
            destination: &'static str,
            host: &'static str,
            spec: RemoteSpec,
        }

        for case in [
            TargetCase {
                input: "dev-box:query-engine",
                destination: "dev-box",
                host: "dev-box",
                spec: RemoteSpec::Session("query-engine".to_owned()),
            },
            TargetCase {
                input: "dev-box:~/code/query-engine",
                destination: "dev-box",
                host: "dev-box",
                spec: RemoteSpec::Path("$HOME/code/query-engine".to_owned()),
            },
            TargetCase {
                input: "dev-box:~",
                destination: "dev-box",
                host: "dev-box",
                spec: RemoteSpec::Path("$HOME".to_owned()),
            },
            TargetCase {
                input: "dev-box:/workspace/hello-world",
                destination: "dev-box",
                host: "dev-box",
                spec: RemoteSpec::Path("/workspace/hello-world".to_owned()),
            },
            TargetCase {
                input: "dev-box:code/query-engine",
                destination: "dev-box",
                host: "dev-box",
                spec: RemoteSpec::Path("code/query-engine".to_owned()),
            },
            TargetCase {
                input: "agent@1.1.1.1:/workspace/hello-world",
                destination: "agent@1.1.1.1",
                host: "1.1.1.1",
                spec: RemoteSpec::Path("/workspace/hello-world".to_owned()),
            },
            TargetCase {
                input: "dev-box:build@2",
                destination: "dev-box",
                host: "dev-box",
                spec: RemoteSpec::Session("build@2".to_owned()),
            },
            TargetCase {
                input: "dev-box:~/code/foo@v2",
                destination: "dev-box",
                host: "dev-box",
                spec: RemoteSpec::Path("$HOME/code/foo@v2".to_owned()),
            },
            TargetCase {
                input: "alice@corp.com@dev-box:query-engine",
                destination: "alice@corp.com@dev-box",
                host: "dev-box",
                spec: RemoteSpec::Session("query-engine".to_owned()),
            },
            TargetCase {
                input: "user@[::1]:/srv/app",
                destination: "user@[::1]",
                host: "::1",
                spec: RemoteSpec::Path("/srv/app".to_owned()),
            },
            TargetCase {
                input: "[::1]:query-engine",
                destination: "[::1]",
                host: "::1",
                spec: RemoteSpec::Session("query-engine".to_owned()),
            },
        ] {
            let target = parse(case.input);
            assert_eq!(target.destination, case.destination, "{}", case.input);
            assert_eq!(target.host_display(), case.host, "{}", case.input);
            assert_eq!(target.spec, case.spec, "{}", case.input);
        }
    }

    #[test]
    fn target_grammar_rejects_malformed_forms() {
        enum ErrorKind {
            Empty,
            MissingColon,
            EmptyTarget,
            EmptyHost,
            UnclosedBracket,
            TildeUser,
        }

        for (input, kind) in [
            ("", ErrorKind::Empty),
            ("dev-box", ErrorKind::MissingColon),
            ("dev-box:", ErrorKind::EmptyTarget),
            (":query-engine", ErrorKind::EmptyHost),
            ("user@:", ErrorKind::EmptyHost),
            ("user@:query-engine", ErrorKind::EmptyHost),
            ("[::1:query-engine", ErrorKind::UnclosedBracket),
            ("dev-box:~alice", ErrorKind::TildeUser),
            ("dev-box:~alice/code", ErrorKind::TildeUser),
        ] {
            let err = RemoteTarget::parse(input).expect_err("target must fail");
            assert!(
                matches!(
                    (kind, err),
                    (ErrorKind::Empty, RemoteTargetError::Empty)
                        | (ErrorKind::MissingColon, RemoteTargetError::MissingColon(_))
                        | (ErrorKind::EmptyTarget, RemoteTargetError::EmptyTarget(_))
                        | (ErrorKind::EmptyHost, RemoteTargetError::EmptyHost(_))
                        | (
                            ErrorKind::UnclosedBracket,
                            RemoteTargetError::UnclosedBracket(_)
                        )
                        | (ErrorKind::TildeUser, RemoteTargetError::TildeUser(_))
                ),
                "{input} returned wrong error"
            );
        }
    }

    #[test]
    fn quote_and_display_are_shell_safe() {
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
        assert_eq!(sh_quote(""), "''");
        assert_eq!(quote_remote_path("$HOME"), "\"$HOME\"");
        assert_eq!(
            quote_remote_path("$HOME/code/query-engine"),
            "\"$HOME\"'/code/query-engine'"
        );
        assert_eq!(quote_remote_path("/abs path"), "'/abs path'");

        let line = display_ssh_command(&ssh_attach_spec(
            &parse("dev-box:query-engine"),
            false,
            None,
            &TermPlan::Keep,
            false,
            None,
        ));
        assert!(line.starts_with("ssh -o ServerAliveInterval=5"), "{line}");
        assert!(line.contains(" -t -- dev-box '"), "{line}");
        assert!(line.ends_with('\''), "{line}");

        let v6 = display_ssh_command(&ssh_attach_spec(
            &parse("[::1]:query-engine"),
            false,
            None,
            &TermPlan::Keep,
            false,
            None,
        ));
        assert!(
            v6.contains(" -- '[::1]' "),
            "bracketed destinations quote against shell globbing: {v6}"
        );
    }

    #[test]
    fn term_plan_selects_keep_copy_or_downgrade() {
        for term in ["alacritty", "xterm-kitty", "xterm-ghostty"] {
            assert!(term_needs_terminfo_copy(term), "{term}");
        }
        for term in ["xterm-256color", "screen-256color", "tmux-256color"] {
            assert!(!term_needs_terminfo_copy(term), "{term}");
        }

        struct TermCase {
            term: Option<&'static str>,
            infocmp: Option<&'static str>,
            expected: TermPlan,
        }

        for case in [
            TermCase {
                term: None,
                infocmp: None,
                expected: TermPlan::Keep,
            },
            TermCase {
                term: Some(""),
                infocmp: None,
                expected: TermPlan::Keep,
            },
            TermCase {
                term: Some("xterm-256color"),
                infocmp: None,
                expected: TermPlan::Keep,
            },
            TermCase {
                term: Some("alacritty"),
                infocmp: Some("ALACRITTY|fake,"),
                expected: TermPlan::Copy {
                    name: "alacritty".to_owned(),
                    source: "ALACRITTY|fake,".to_owned(),
                },
            },
            TermCase {
                term: Some("xterm-kitty"),
                infocmp: None,
                expected: TermPlan::Downgrade,
            },
            TermCase {
                term: Some("xterm-ghostty"),
                infocmp: Some("  "),
                expected: TermPlan::Downgrade,
            },
        ] {
            assert_eq!(
                term_plan_from(case.term, |_| case.infocmp.map(ToOwned::to_owned)),
                case.expected
            );
        }
    }

    #[test]
    fn ssh_attach_spec_compiles_session_path_flags_control_and_term() {
        struct SpecCase {
            name: &'static str,
            target: &'static str,
            no_resume: bool,
            mux: Option<MuxName>,
            term: TermPlan,
            truecolor: bool,
            control: Option<&'static Path>,
            destination_index: usize,
            snippet_contains: &'static [&'static str],
        }

        for case in [
            SpecCase {
                name: "session attach",
                target: "dev-box:query-engine",
                no_resume: false,
                mux: None,
                term: TermPlan::Keep,
                truecolor: false,
                control: None,
                destination_index: 10,
                snippet_contains: &[
                    "command -v rimz",
                    "rimz not found on dev-box",
                    "exit 127",
                    "exec rimz attach --attach -- 'query-engine'",
                ],
            },
            SpecCase {
                name: "path start",
                target: "dev-box:~/code/query-engine",
                no_resume: false,
                mux: None,
                term: TermPlan::Keep,
                truecolor: false,
                control: None,
                destination_index: 10,
                snippet_contains: &["exec rimz start --attach -- \"$HOME\"'/code/query-engine'"],
            },
            SpecCase {
                name: "no resume and mux",
                target: "dev-box:query-engine",
                no_resume: true,
                mux: Some(MuxName::Tmux),
                term: TermPlan::Keep,
                truecolor: false,
                control: None,
                destination_index: 10,
                snippet_contains: &["exec rimz attach --attach --no-resume --mux tmux -- "],
            },
            SpecCase {
                name: "control master",
                target: "dev-box:query-engine",
                no_resume: false,
                mux: None,
                term: TermPlan::Keep,
                truecolor: false,
                control: Some(Path::new("/tmp/rimz.sock")),
                destination_index: 14,
                snippet_contains: &["exec rimz attach --attach -- 'query-engine'"],
            },
            SpecCase {
                name: "term downgrade",
                target: "dev-box:query-engine",
                no_resume: false,
                mux: None,
                term: TermPlan::Downgrade,
                truecolor: false,
                control: None,
                destination_index: 10,
                snippet_contains: &["export TERM=xterm-256color; exec rimz"],
            },
            SpecCase {
                name: "truecolor keep",
                target: "dev-box:query-engine",
                no_resume: false,
                mux: None,
                term: TermPlan::Keep,
                truecolor: true,
                control: None,
                destination_index: 10,
                snippet_contains: &["export COLORTERM=truecolor; exec rimz"],
            },
            SpecCase {
                name: "truecolor and term downgrade",
                target: "dev-box:query-engine",
                no_resume: false,
                mux: None,
                term: TermPlan::Downgrade,
                truecolor: true,
                control: None,
                destination_index: 10,
                snippet_contains: &[
                    "export COLORTERM=truecolor; export TERM=xterm-256color; exec rimz",
                ],
            },
            SpecCase {
                name: "term copy",
                target: "dev-box:query-engine",
                no_resume: false,
                mux: None,
                term: TermPlan::Copy {
                    name: "alacritty".to_owned(),
                    source: "ALACRITTY|fake,".to_owned(),
                },
                truecolor: false,
                control: None,
                destination_index: 10,
                snippet_contains: &[concat!(
                    "export TERM=xterm-256color; printf '%s\\n' 'ALACRITTY|fake,' | ",
                    "tic -x - 2>/dev/null && export TERM='alacritty'; exec rimz"
                )],
            },
        ] {
            let spec = ssh_attach_spec(
                &parse(case.target),
                case.no_resume,
                case.mux,
                &case.term,
                case.truecolor,
                case.control,
            );
            assert_eq!(spec.program, "ssh", "{}", case.name);
            assert_eq!(
                spec.args[..8],
                [
                    "-o",
                    "ServerAliveInterval=5",
                    "-o",
                    "ServerAliveCountMax=3",
                    "-o",
                    "ConnectTimeout=10",
                    "-o",
                    "Compression=yes",
                ],
                "{}",
                case.name
            );
            if case.control.is_some() {
                assert_eq!(
                    spec.args[8..12],
                    [
                        "-o",
                        "ControlMaster=auto",
                        "-o",
                        "ControlPath=/tmp/rimz.sock",
                    ],
                    "{}",
                    case.name
                );
            }
            assert_eq!(spec.args[case.destination_index - 2], "-t", "{}", case.name);
            assert_eq!(spec.args[case.destination_index - 1], "--", "{}", case.name);
            assert_eq!(
                spec.args[case.destination_index], "dev-box",
                "{}",
                case.name
            );
            assert_eq!(
                spec.args.len(),
                case.destination_index + 2,
                "snippet is a single argv element: {}",
                case.name
            );
            let snippet = spec.args.last().expect("snippet");
            assert!(
                snippet.starts_with("PATH=\"$HOME/.cargo/bin"),
                "{}",
                case.name
            );
            for needle in case.snippet_contains {
                assert!(
                    snippet.contains(needle),
                    "{} missing {needle}: {snippet}",
                    case.name
                );
            }
        }
    }

    #[test]
    fn verdict_and_backoff_classify_reconnects() {
        let base = Duration::from_secs(1);
        let cap = Duration::from_secs(30);
        let delays: Vec<Duration> = (0..7)
            .map(|failures| backoff(failures, base, cap))
            .collect();
        assert_eq!(
            delays,
            [
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(16),
                Duration::from_secs(30),
                Duration::from_secs(30),
            ]
        );

        let policy = ReconnectPolicy::default();
        assert_eq!(verdict(Some(0), true, 0, &policy), Verdict::CleanExit);
        assert_eq!(
            verdict(Some(SSH_TRANSPORT_EXIT), true, 2, &policy),
            Verdict::Retry {
                delay: Duration::from_secs(4)
            }
        );
        assert_eq!(
            verdict(Some(SSH_TRANSPORT_EXIT), true, 30, &policy),
            Verdict::Retry {
                delay: Duration::from_secs(30)
            }
        );
        assert_eq!(
            verdict(Some(SSH_TRANSPORT_EXIT), false, 0, &policy),
            Verdict::Fatal {
                code: SSH_TRANSPORT_EXIT
            }
        );
        assert_eq!(
            verdict(Some(REMOTE_RIMZ_MISSING_EXIT), true, 0, &policy),
            Verdict::Fatal {
                code: REMOTE_RIMZ_MISSING_EXIT
            }
        );
        assert_eq!(verdict(None, true, 0, &policy), Verdict::Fatal { code: 1 });
    }

    #[test]
    fn reconnect_state_settles_established_sessions_and_failures() {
        let policy = ReconnectPolicy::default();
        let mut state = ReconnectState::new(policy);

        assert_eq!(
            state.settle(Some(SSH_TRANSPORT_EXIT), false),
            Verdict::Fatal {
                code: SSH_TRANSPORT_EXIT
            }
        );
        assert_eq!(state.consecutive_failures(), 0);

        assert_eq!(
            state.settle(Some(SSH_TRANSPORT_EXIT), true),
            Verdict::Retry {
                delay: Duration::from_secs(1)
            }
        );
        assert_eq!(state.consecutive_failures(), 1);

        assert_eq!(
            state.settle(Some(SSH_TRANSPORT_EXIT), false),
            Verdict::Retry {
                delay: Duration::from_secs(2)
            }
        );
        assert_eq!(state.consecutive_failures(), 2);

        assert_eq!(
            state.settle(Some(REMOTE_RIMZ_MISSING_EXIT), true),
            Verdict::Fatal {
                code: REMOTE_RIMZ_MISSING_EXIT
            }
        );
        assert_eq!(state.consecutive_failures(), 0);
    }
}
