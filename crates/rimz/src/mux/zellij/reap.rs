//! Pre-attach retirement of orphaned Zellij clients from one remote lineage.
//!
//! Zellij 0.44 can reuse a client id before stale queued removals for the old
//! client have drained. A replacement remote attach therefore retires its
//! same-device predecessor and waits for `list-clients` to observe the removal
//! before registering the new client.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::time::{Duration, Instant};

use crate::mux::{ClientFocusOptions, MuxBackend, MuxErr, Result as MuxResult};
use crate::proc::{self, ProcInfo};
use crate::remote::REMOTE_LINEAGE_ENV;

const REAP_TIMEOUT: Duration = Duration::from_secs(2);
const TERM_GRACE: Duration = Duration::from_millis(500);
const PROCESS_POLL_STEP: Duration = Duration::from_millis(50);
const CLIENT_POLL_STEP: Duration = Duration::from_millis(100);
const CLIENT_QUERY_TIMEOUT: Duration = Duration::from_millis(250);

/// Evidence collected while retiring same-lineage predecessor clients.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReapOutcome {
    pub killed_pids: Vec<u32>,
    pub pre_clients: Option<usize>,
    pub post_clients: Option<usize>,
    pub settled: bool,
    pub timed_out: bool,
    pub errors: Vec<String>,
}

/// Retire every same-lineage `zellij attach --create <session>` process and
/// wait until the server's human-client count proves their removals drained.
/// Failures stay in the returned evidence so the attach seam can proceed while
/// recording the degraded cleanup.
pub fn reap_lineage_clients(
    engine: &dyn MuxBackend,
    session_name: &str,
    lineage: &str,
) -> MuxResult<ReapOutcome> {
    let started = Instant::now();
    let deadline = started + REAP_TIMEOUT;
    let processes = proc::list_processes();
    let own_pid = std::process::id();
    let protected = protected_processes(&processes, own_pid);
    let own_uid = proc::own_uid();
    let victims = select_lineage_clients(
        &processes,
        MatchContext {
            own_uid,
            protected: &protected,
            session_name,
            lineage,
        },
        proc::comm,
        proc::argv,
        proc::env_var,
    );

    if victims.is_empty() {
        return Ok(ReapOutcome {
            settled: true,
            ..ReapOutcome::default()
        });
    }

    let mut outcome = ReapOutcome::default();
    let pre_clients = query_client_count_until(engine, session_name, deadline)?;
    outcome.pre_clients = Some(pre_clients);
    let expected_drop = victims.len();

    let mut signaled = Vec::new();
    for pid in victims {
        let Some(start_token) = proc::process_start_token(pid) else {
            outcome
                .errors
                .push(format!("pid {pid} vanished before it could be identified"));
            continue;
        };
        if !pid_matches(pid, session_name, lineage) {
            outcome
                .errors
                .push(format!("pid {pid} changed before SIGTERM"));
            continue;
        }
        match send_signal(pid, ReapSignal::Term) {
            Ok(()) => {
                outcome.killed_pids.push(pid);
                signaled.push((pid, start_token));
            }
            Err(err) => outcome
                .errors
                .push(format!("sending SIGTERM to pid {pid}: {err}")),
        }
    }

    let grace_deadline = (Instant::now() + TERM_GRACE).min(deadline);
    while Instant::now() < grace_deadline
        && signaled
            .iter()
            .any(|(pid, token)| same_process_is_alive(*pid, token))
    {
        std::thread::sleep(
            PROCESS_POLL_STEP.min(grace_deadline.saturating_duration_since(Instant::now())),
        );
    }
    for (pid, token) in &signaled {
        if same_process_is_alive(*pid, token)
            && let Err(err) = send_signal(*pid, ReapSignal::Kill)
        {
            outcome
                .errors
                .push(format!("sending SIGKILL to pid {pid}: {err}"));
        }
    }

    let target = pre_clients.saturating_sub(expected_drop);
    let mut last_poll_error = None;
    while Instant::now() < deadline {
        match query_client_count(engine, session_name, deadline) {
            Ok(count) => {
                outcome.post_clients = Some(count);
                if count <= target {
                    outcome.settled = true;
                    return Ok(outcome);
                }
            }
            Err(err) => last_poll_error = Some(err),
        }
        let now = Instant::now();
        if now < deadline {
            std::thread::sleep(CLIENT_POLL_STEP.min(deadline - now));
        }
    }
    if let Some(err) = last_poll_error {
        outcome
            .errors
            .push(format!("polling the post-reap client count: {err}"));
    }
    outcome.timed_out = true;
    Ok(outcome)
}

fn query_client_count(
    engine: &dyn MuxBackend,
    session_name: &str,
    deadline: Instant,
) -> MuxResult<usize> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(MuxErr::Timeout {
            program: engine.name().to_string(),
            args: format!("--session {session_name} action list-clients"),
            seconds: REAP_TIMEOUT.as_secs(),
        });
    }
    engine
        .client_view(ClientFocusOptions {
            session_name: Some(session_name.to_owned()),
            command_timeout: Some(CLIENT_QUERY_TIMEOUT.min(remaining)),
        })
        .map(|view| view.presence.human_clients)
}

fn query_client_count_until(
    engine: &dyn MuxBackend,
    session_name: &str,
    deadline: Instant,
) -> MuxResult<usize> {
    query_client_count_until_with(deadline, CLIENT_POLL_STEP, |deadline| {
        query_client_count(engine, session_name, deadline)
    })
}

fn query_client_count_until_with(
    deadline: Instant,
    poll_step: Duration,
    mut query: impl FnMut(Instant) -> MuxResult<usize>,
) -> MuxResult<usize> {
    loop {
        match query(deadline) {
            Ok(count) => return Ok(count),
            Err(err) => {
                let now = Instant::now();
                if now >= deadline {
                    return Err(err);
                }
                std::thread::sleep(poll_step.min(deadline - now));
            }
        }
    }
}

fn protected_processes(processes: &[ProcInfo], own_pid: u32) -> HashSet<u32> {
    let parents = processes
        .iter()
        .map(|process| (process.pid, process.ppid))
        .collect::<HashMap<_, _>>();
    let mut protected = HashSet::new();
    let mut cursor = own_pid;
    while cursor != 0 && protected.insert(cursor) {
        let Some(parent) = parents.get(&cursor).copied() else {
            break;
        };
        cursor = parent;
    }
    protected
}

struct MatchContext<'a> {
    own_uid: Option<u32>,
    protected: &'a HashSet<u32>,
    session_name: &'a str,
    lineage: &'a str,
}

fn select_lineage_clients<C, A, E>(
    processes: &[ProcInfo],
    context: MatchContext<'_>,
    mut comm_lookup: C,
    mut argv_lookup: A,
    mut env_lookup: E,
) -> Vec<u32>
where
    C: FnMut(u32) -> Option<String>,
    A: FnMut(u32) -> Option<Vec<OsString>>,
    E: FnMut(u32, &str) -> Option<String>,
{
    let mut victims = processes
        .iter()
        .filter(|process| context.own_uid.is_none_or(|uid| process.real_uid == uid))
        .filter(|process| !context.protected.contains(&process.pid))
        .filter(|process| process.cmdline.contains("zellij"))
        .filter(|process| comm_lookup(process.pid).as_deref() == Some("zellij"))
        .filter(|process| {
            argv_lookup(process.pid)
                .as_deref()
                .is_some_and(|argv| attach_argv_matches(argv, context.session_name))
        })
        .filter(|process| {
            env_lookup(process.pid, REMOTE_LINEAGE_ENV).as_deref() == Some(context.lineage)
        })
        .map(|process| process.pid)
        .collect::<Vec<_>>();
    victims.sort_unstable();
    victims
}

fn attach_argv_matches(argv: &[OsString], session_name: &str) -> bool {
    argv.get(1).and_then(|arg| arg.to_str()) == Some("attach")
        && argv.get(2).and_then(|arg| arg.to_str()) == Some("--create")
        && argv.get(3).and_then(|arg| arg.to_str()) == Some(session_name)
}

fn pid_matches(pid: u32, session_name: &str, lineage: &str) -> bool {
    proc::comm(pid).as_deref() == Some("zellij")
        && proc::argv(pid)
            .as_deref()
            .is_some_and(|argv| attach_argv_matches(argv, session_name))
        && proc::env_var(pid, REMOTE_LINEAGE_ENV).as_deref() == Some(lineage)
}

fn same_process_is_alive(pid: u32, start_token: &str) -> bool {
    proc::process_start_token(pid).as_deref() == Some(start_token)
}

#[derive(Clone, Copy)]
enum ReapSignal {
    Term,
    Kill,
}

#[cfg(unix)]
fn send_signal(pid: u32, signal: ReapSignal) -> Result<(), String> {
    use nix::errno::Errno;
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    let signal = match signal {
        ReapSignal::Term => Signal::SIGTERM,
        ReapSignal::Kill => Signal::SIGKILL,
    };
    match kill(Pid::from_raw(pid as i32), signal) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(err) => Err(err.to_string()),
    }
}

#[cfg(not(unix))]
fn send_signal(_pid: u32, _signal: ReapSignal) -> Result<(), String> {
    Err("process signals are unavailable on this platform".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, ppid: u32, uid: u32, cmdline: &str) -> ProcInfo {
        ProcInfo {
            pid,
            ppid,
            real_uid: uid,
            cmdline: cmdline.to_owned(),
        }
    }

    fn zellij_argv(session: &str) -> Vec<OsString> {
        ["/usr/bin/zellij", "attach", "--create", session]
            .into_iter()
            .map(OsString::from)
            .collect()
    }

    #[test]
    fn matcher_selects_only_same_lineage_clients_outside_own_ancestry() {
        let processes = vec![
            process(10, 1, 1000, "zellij attach --create room"),
            process(20, 10, 1000, "zellij attach --create room"),
            process(30, 20, 1000, "rimz attach --attach room"),
            process(40, 30, 1000, "zellij attach --create room"),
            process(41, 30, 1000, "zellij attach --create room"),
            process(42, 30, 1000, "zellij attach --create other"),
            process(43, 30, 2000, "zellij attach --create room"),
        ];
        let protected = protected_processes(&processes, 30);
        let lineage = |pid| match pid {
            10 | 20 | 30 | 40 | 42 | 43 => Some("same".to_owned()),
            41 => Some("other".to_owned()),
            _ => None,
        };
        let argv = |pid| match pid {
            42 => Some(zellij_argv("other")),
            _ => Some(zellij_argv("room")),
        };

        assert_eq!(
            select_lineage_clients(
                &processes,
                MatchContext {
                    own_uid: Some(1000),
                    protected: &protected,
                    session_name: "room",
                    lineage: "same",
                },
                |_| Some("zellij".to_owned()),
                argv,
                |pid, _| lineage(pid),
            ),
            vec![40]
        );
    }

    #[test]
    fn matcher_requires_the_rimz_attach_argv_shape() {
        let processes = vec![
            process(1, 0, 1000, "zellij attach room"),
            process(2, 0, 1000, "zellij --session room action list-clients"),
        ];

        assert!(
            select_lineage_clients(
                &processes,
                MatchContext {
                    own_uid: Some(1000),
                    protected: &HashSet::new(),
                    session_name: "room",
                    lineage: "same",
                },
                |_| Some("zellij".to_owned()),
                |pid| match pid {
                    1 => Some(["zellij", "attach", "room"].map(OsString::from).to_vec()),
                    _ => Some(
                        ["zellij", "--session", "room", "action", "list-clients"]
                            .map(OsString::from)
                            .to_vec(),
                    ),
                },
                |_, _| Some("same".to_owned()),
            )
            .is_empty()
        );
    }

    #[test]
    fn initial_client_count_retries_transient_mux_errors() {
        let mut attempts = 0;
        let count = query_client_count_until_with(
            Instant::now() + Duration::from_secs(1),
            Duration::ZERO,
            |_| {
                attempts += 1;
                if attempts < 3 {
                    Err(MuxErr::Output {
                        program: "zellij".to_owned(),
                        reason: "transient list-clients failure".to_owned(),
                    })
                } else {
                    Ok(1)
                }
            },
        )
        .expect("client count after transient failures");

        assert_eq!(count, 1);
        assert_eq!(attempts, 3);
    }
}
