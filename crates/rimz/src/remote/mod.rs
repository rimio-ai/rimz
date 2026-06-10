//! SSH remote attach: the `[user@]host:<session-or-path>` target grammar, the
//! guarded `ssh` command it compiles to, and the autossh-style reconnect
//! policy.
//!
//! `rimz remote connect` makes the local rimz a thin SSH launcher and link
//! supervisor: everything room-shaped — workspace resolution, session birth,
//! sidebar, health gate — runs on the remote host's own `rimz`, and the room
//! renders here because `ssh -t` carries the terminal. This module is pure:
//! it parses targets, builds `CommandSpec`s, and decides reconnects; the cli
//! owns process I/O.

pub mod aliases;
pub mod link;

use std::path::Path;
use std::time::Duration;

use crate::ids::MuxName;
use crate::mux::CommandSpec;

/// Binary override for tests (`tests/fixtures/ssh-trace`), mirroring
/// `RIMZ_ZELLIJ_BIN` — the single chokepoint every ssh invocation resolves
/// through.
pub const SSH_BIN_ENV: &str = "RIMZ_SSH_BIN";

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

/// Compile a remote target into the full ssh invocation.
///
/// The keepalive options give the reconnect loop its dead-link signal
/// (~15s detection); `-t` forces a remote PTY so the room renders
/// interactively; `--` stops ssh option parsing before a destination that
/// could look flag-ish. The final argument is the guarded snippet — one argv
/// element; ssh hands it to the remote login shell as the command string.
pub fn ssh_attach_spec(
    target: &RemoteTarget,
    no_resume: bool,
    mux: Option<MuxName>,
) -> CommandSpec {
    ssh_attach_spec_with_control(target, no_resume, mux, None)
}

/// [`ssh_attach_spec`] plus optional ControlMaster setup for the supervised
/// remote path. `None` is byte-identical to the legacy invocation.
pub fn ssh_attach_spec_with_control(
    target: &RemoteTarget,
    no_resume: bool,
    mux: Option<MuxName>,
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
        ])
        .args(control.into_iter().flat_map(link::control_options))
        .args(["-t", "--"])
        .arg(target.destination.clone())
        .arg(guarded_snippet(target, no_resume, mux))
}

/// The single remote shell command: repair the non-login-shell PATH, fail
/// with the install fix when the host has no `rimz`, then exec into the room.
fn guarded_snippet(target: &RemoteTarget, no_resume: bool, mux: Option<MuxName>) -> String {
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
    let not_found = sh_quote(&format!(
        "rimz not found on {} — install: cargo install rimz",
        target.host,
    ));
    format!(
        "{}; \
         command -v rimz >/dev/null 2>&1 || {{ echo {not_found} >&2; exit {code}; }}; \
         exec {rimz} -- {arg}",
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
/// shell would split or expand. (`cli`'s `command_display` joins with bare
/// spaces, which is fine for mux specs but garbles the snippet argument.)
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
            delay: backoff(consecutive_failures, policy),
        },
        Some(code) => Verdict::Fatal { code },
        // Signal-death: something killed ssh deliberately; don't fight it.
        None => Verdict::Fatal { code: 1 },
    }
}

fn backoff(consecutive_failures: u32, policy: &ReconnectPolicy) -> Duration {
    let factor = 2u32.saturating_pow(consecutive_failures.min(16));
    policy
        .backoff_base
        .saturating_mul(factor)
        .min(policy.backoff_cap)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> RemoteTarget {
        RemoteTarget::parse(input).expect("target parses")
    }

    #[test]
    fn session_target_parses() {
        let target = parse("dev-box:query-engine");
        assert_eq!(target.destination, "dev-box");
        assert_eq!(target.host_display(), "dev-box");
        assert_eq!(target.spec, RemoteSpec::Session("query-engine".to_owned()));
    }

    #[test]
    fn tilde_path_normalizes_to_home() {
        let target = parse("dev-box:~/code/query-engine");
        assert_eq!(
            target.spec,
            RemoteSpec::Path("$HOME/code/query-engine".to_owned())
        );
    }

    #[test]
    fn bare_tilde_is_a_path() {
        assert_eq!(
            parse("dev-box:~").spec,
            RemoteSpec::Path("$HOME".to_owned())
        );
    }

    #[test]
    fn absolute_and_relative_paths_classify_as_paths() {
        assert_eq!(
            parse("dev-box:/workspace/hello-world").spec,
            RemoteSpec::Path("/workspace/hello-world".to_owned())
        );
        assert_eq!(
            parse("dev-box:code/query-engine").spec,
            RemoteSpec::Path("code/query-engine".to_owned())
        );
    }

    #[test]
    fn user_prefix_rides_the_destination() {
        let target = parse("agent@1.1.1.1:/workspace/hello-world");
        assert_eq!(target.destination, "agent@1.1.1.1");
        assert_eq!(target.host_display(), "1.1.1.1");
    }

    #[test]
    fn target_keeps_its_own_at_sign() {
        // The first `:` ends the host; an `@` after it belongs to the target.
        let target = parse("dev-box:build@2");
        assert_eq!(target.destination, "dev-box");
        assert_eq!(target.spec, RemoteSpec::Session("build@2".to_owned()));
        assert_eq!(
            parse("dev-box:~/code/foo@v2").spec,
            RemoteSpec::Path("$HOME/code/foo@v2".to_owned())
        );
    }

    #[test]
    fn user_splits_at_the_last_at_sign() {
        // ssh's rule: usernames can carry their own `@` (AD-style).
        let target = parse("alice@corp.com@dev-box:query-engine");
        assert_eq!(target.destination, "alice@corp.com@dev-box");
        assert_eq!(target.host_display(), "dev-box");
    }

    #[test]
    fn bracketed_ipv6_hosts_parse() {
        let target = parse("user@[::1]:/srv/app");
        assert_eq!(target.destination, "user@[::1]");
        assert_eq!(target.host_display(), "::1");
        assert_eq!(target.spec, RemoteSpec::Path("/srv/app".to_owned()));

        let target = parse("[::1]:query-engine");
        assert_eq!(target.destination, "[::1]");
        assert_eq!(target.spec, RemoteSpec::Session("query-engine".to_owned()));
    }

    #[test]
    fn malformed_targets_name_the_fix() {
        assert!(matches!(
            RemoteTarget::parse(""),
            Err(RemoteTargetError::Empty)
        ));
        assert!(matches!(
            RemoteTarget::parse("dev-box"),
            Err(RemoteTargetError::MissingColon(_))
        ));
        assert!(matches!(
            RemoteTarget::parse("dev-box:"),
            Err(RemoteTargetError::EmptyTarget(_))
        ));
        assert!(matches!(
            RemoteTarget::parse(":query-engine"),
            Err(RemoteTargetError::EmptyHost(_))
        ));
        assert!(matches!(
            RemoteTarget::parse("user@:query-engine"),
            Err(RemoteTargetError::EmptyHost(_))
        ));
        assert!(matches!(
            RemoteTarget::parse("[::1:query-engine"),
            Err(RemoteTargetError::UnclosedBracket(_))
        ));
        // `~user` cannot expand through the quoted snippet — fail with the fix.
        assert!(matches!(
            RemoteTarget::parse("dev-box:~alice/code"),
            Err(RemoteTargetError::TildeUser(_))
        ));
        assert!(matches!(
            RemoteTarget::parse("dev-box:~alice"),
            Err(RemoteTargetError::TildeUser(_))
        ));
    }

    #[test]
    fn sh_quote_escapes_shell_words() {
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
        assert_eq!(sh_quote(""), "''");
    }

    #[test]
    fn quote_remote_path_expands_home_outside_quotes() {
        assert_eq!(quote_remote_path("$HOME"), "\"$HOME\"");
        assert_eq!(
            quote_remote_path("$HOME/code/query-engine"),
            "\"$HOME\"'/code/query-engine'"
        );
        assert_eq!(quote_remote_path("/abs path"), "'/abs path'");
    }

    #[test]
    fn session_spec_compiles_to_remote_attach() {
        let target = parse("dev-box:query-engine");
        let spec = ssh_attach_spec(&target, false, None);
        assert_eq!(spec.program, "ssh");
        assert_eq!(
            spec.args[..8],
            [
                "-o",
                "ServerAliveInterval=5",
                "-o",
                "ServerAliveCountMax=3",
                "-o",
                "ConnectTimeout=10",
                "-t",
                "--",
            ]
        );
        assert_eq!(spec.args[8], "dev-box");
        let snippet = &spec.args[9];
        assert_eq!(spec.args.len(), 10, "snippet is a single argv element");
        assert!(snippet.starts_with("PATH=\"$HOME/.cargo/bin"));
        assert!(snippet.contains("command -v rimz"));
        assert!(snippet.contains("rimz not found on dev-box"));
        assert!(snippet.contains("exit 127"));
        assert!(snippet.ends_with("exec rimz attach --attach -- 'query-engine'"));
    }

    #[test]
    fn supervised_spec_adds_controlmaster_without_changing_the_plain_spec() {
        let target = parse("dev-box:query-engine");
        let plain = ssh_attach_spec(&target, false, None);
        let control =
            ssh_attach_spec_with_control(&target, false, None, Some(Path::new("/tmp/rimz.sock")));

        assert_eq!(plain.args[..6], control.args[..6]);
        assert_eq!(
            control.args[6..10],
            [
                "-o",
                "ControlMaster=auto",
                "-o",
                "ControlPath=/tmp/rimz.sock",
            ]
        );
        assert_eq!(control.args[10], "-t");
        assert_eq!(control.args[11], "--");
        assert_eq!(control.args[12], "dev-box");
    }

    #[test]
    fn path_spec_compiles_to_remote_start() {
        let target = parse("dev-box:~/code/query-engine");
        let spec = ssh_attach_spec(&target, false, None);
        let snippet = spec.args.last().expect("snippet");
        assert!(snippet.ends_with("exec rimz start --attach -- \"$HOME\"'/code/query-engine'"));
    }

    #[test]
    fn no_resume_and_mux_ride_the_remote_invocation() {
        let target = parse("dev-box:query-engine");
        let spec = ssh_attach_spec(&target, true, Some(MuxName::Tmux));
        let snippet = spec.args.last().expect("snippet");
        assert!(snippet.contains("exec rimz attach --attach --no-resume --mux tmux -- "));
    }

    #[test]
    fn display_ssh_command_is_pasteable() {
        let target = parse("dev-box:query-engine");
        let spec = ssh_attach_spec(&target, false, None);
        let line = display_ssh_command(&spec);
        assert!(line.starts_with("ssh -o ServerAliveInterval=5"));
        assert!(line.contains(" -t -- dev-box '"));
        assert!(line.ends_with("'"));

        let v6 = ssh_attach_spec(&parse("[::1]:query-engine"), false, None);
        assert!(
            display_ssh_command(&v6).contains(" -- '[::1]' "),
            "bracketed destinations quote against shell globbing"
        );
    }

    #[test]
    fn verdict_classifies_session_exits() {
        let policy = ReconnectPolicy::default();
        assert_eq!(verdict(Some(0), true, 0, &policy), Verdict::CleanExit);
        assert_eq!(verdict(Some(0), false, 0, &policy), Verdict::CleanExit);
        // Transport loss on an established link retries with growing backoff.
        assert_eq!(
            verdict(Some(SSH_TRANSPORT_EXIT), true, 0, &policy),
            Verdict::Retry {
                delay: Duration::from_secs(1)
            }
        );
        assert_eq!(
            verdict(Some(SSH_TRANSPORT_EXIT), true, 2, &policy),
            Verdict::Retry {
                delay: Duration::from_secs(4)
            }
        );
        // …capped.
        assert_eq!(
            verdict(Some(SSH_TRANSPORT_EXIT), true, 30, &policy),
            Verdict::Retry {
                delay: Duration::from_secs(30)
            }
        );
        // A transport failure before any session established is fatal — auth
        // failures and unknown hosts never become a prompt loop.
        assert_eq!(
            verdict(Some(SSH_TRANSPORT_EXIT), false, 0, &policy),
            Verdict::Fatal {
                code: SSH_TRANSPORT_EXIT
            }
        );
        // Remote rimz missing / remote room failures are not link problems.
        assert_eq!(
            verdict(Some(REMOTE_RIMZ_MISSING_EXIT), true, 0, &policy),
            Verdict::Fatal {
                code: REMOTE_RIMZ_MISSING_EXIT
            }
        );
        assert_eq!(
            verdict(Some(1), true, 0, &policy),
            Verdict::Fatal { code: 1 }
        );
        // Signal-death stops the loop.
        assert_eq!(verdict(None, true, 0, &policy), Verdict::Fatal { code: 1 });
    }
}
