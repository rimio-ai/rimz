//! Run test-executing xtask subcommands in a session of their own.
//!
//! Re-enters the running xtask binary at a hidden first argument carrying the
//! outer process's pid (`__in-session=<pid>`) so the inner process calls
//! `setsid` before any test or build child starts, dropping any controlling
//! terminal. The outer process forwards non-ignored `SIGINT`, `SIGTERM`, and
//! `SIGHUP` to the inner's process group, drains live members of that group
//! (ignoring exited-but-unreaped zombies on Linux via `/proc`), and terminates
//! by that same signal (or mirrors the inner's exit status when not signalled).
//!
//! On Linux, signals already ignored (`SIG_IGN`) when the outer starts are read
//! from `/proc/self/status` (`SigIgn`), left unhandled by the outer, and
//! inherited as ignored by the inner session. Non-Linux hosts have no safe API
//! in std, `rustix`, or `signal-hook` to inspect an inherited disposition
//! without replacing it, so all three signals are registered as forwarded there.
//!
//! Inside the session, a parent-pid watch checks `getppid() == Some(outer_pid)`
//! before starting the task and polls periodically thereafter, sending
//! `SIGTERM` to its own process group if the outer dies without forwarding (for
//! example `SIGKILL` on an agent tool timeout or a death before the watch starts).

use std::os::unix::process::ExitStatusExt;
use std::process::{Command, ExitStatus};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use rustix::process::{Pid, Signal, kill_process, kill_process_group, test_kill_process_group};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

use crate::sandbox;

#[cfg(test)]
mod tests;

pub(crate) const IN_SESSION_ARG: &str = "__in-session";

const IN_SESSION_ARG_PREFIX: &str = "__in-session=";
const FORWARDED_SIGNALS: [i32; 3] = [SIGINT, SIGTERM, SIGHUP];
const PARENT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const GROUP_DRAIN_TIMEOUT: Duration = Duration::from_secs(15);
const GROUP_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Format the hidden first argument carrying the outer process's pid.
pub(crate) fn in_session_arg(outer_pid: Pid) -> String {
    format!("{IN_SESSION_ARG_PREFIX}{}", outer_pid.as_raw_nonzero())
}

/// Parse a leading hidden in-session argument (`__in-session=<pid>`), returning
/// `Ok(None)` when absent and `Err` when malformed.
pub(crate) fn parse_in_session(args: &[String]) -> Result<Option<(Pid, &[String])>> {
    let Some(first) = args.first() else {
        return Ok(None);
    };
    if first == IN_SESSION_ARG {
        bail!(
            "in-session mode `{first}` requires a positive outer pid (`{IN_SESSION_ARG_PREFIX}<pid>`)"
        );
    }
    let Some(raw_pid) = first.strip_prefix(IN_SESSION_ARG_PREFIX) else {
        return Ok(None);
    };
    let pid = raw_pid
        .bytes()
        .all(|byte| byte.is_ascii_digit())
        .then(|| raw_pid.parse::<i32>().ok())
        .flatten()
        .filter(|&value| value > 0)
        .and_then(Pid::from_raw)
        .with_context(|| format!("invalid outer pid in `{first}`: expected a positive integer"))?;
    Ok(Some((pid, &args[1..])))
}

/// Enter a new session and arm the parent-death watch against `outer_pid`.
///
/// If the outer process already died before `enter` ran, terminates this
/// process's new process group with `SIGTERM` immediately rather than running
/// the task detached.
pub(crate) fn enter(outer_pid: Pid) -> Result<()> {
    rustix::process::setsid().context("creating a new session for xtask")?;
    let my_pid = rustix::process::getpid();
    if rustix::process::getppid() != Some(outer_pid) {
        let _ = kill_process_group(my_pid, Signal::TERM);
        thread::sleep(PARENT_POLL_INTERVAL);
        bail!(
            "outer xtask (pid {}) exited before inner session started",
            outer_pid.as_raw_nonzero()
        );
    }
    thread::Builder::new()
        .name("xtask-parent-watch".to_owned())
        .spawn(move || {
            while rustix::process::getppid() == Some(outer_pid) {
                thread::sleep(PARENT_POLL_INTERVAL);
            }
            let _ = kill_process_group(my_pid, Signal::TERM);
        })
        .context("starting xtask parent-death watch")?;
    Ok(())
}

/// Re-enter the running xtask image in a new session with `args` unchanged.
pub(crate) fn run_in_own_session(args: &[String]) -> Result<i32> {
    let mut command = Command::new(sandbox::self_executable()?);
    command
        .arg(in_session_arg(rustix::process::getpid()))
        .args(args);
    run_outer_command(command)
}

/// Spawn `command` as the inner process, forward non-ignored interrupts to its
/// process group, wait for it to finish, and return the exit code (or terminate
/// by the forwarded signal).
fn run_outer_command(mut command: Command) -> Result<i32> {
    let to_forward = signals_to_forward();
    if to_forward.is_empty() {
        let mut child = command.spawn().context("spawning in-session xtask")?;
        let status = child.wait().context("waiting for in-session xtask")?;
        return Ok(exit_code_from_status(status));
    }

    let mut signals = Signals::new(&to_forward).context("installing xtask signal forwarder")?;
    if let Some(sig) = signals.pending().next() {
        drop(signals);
        return Ok(terminate_by_signal(sig));
    }

    let mut child = command.spawn().context("spawning in-session xtask")?;
    let child_pid = i32::try_from(child.id())
        .ok()
        .and_then(Pid::from_raw)
        .context("in-session xtask returned an invalid pid")?;

    let handle = signals.handle();
    let last_signal = AtomicI32::new(0);
    let child_reaped = AtomicBool::new(false);

    let status = thread::scope(|scope| {
        let forwarder = match thread::Builder::new()
            .name("xtask-signal-forward".to_owned())
            .spawn_scoped(scope, || {
                for sig in signals.forever() {
                    last_signal.store(sig, Ordering::Release);
                    forward_signal(child_pid, sig, &child_reaped);
                }
            }) {
            Ok(forwarder) => forwarder,
            Err(err) => {
                let _ = kill_process(child_pid, Signal::TERM);
                let _ = child.wait();
                return Err(err).context("starting xtask signal forwarder");
            }
        };

        let status = child.wait().context("waiting for in-session xtask");
        child_reaped.store(true, Ordering::Release);
        if last_signal.load(Ordering::Acquire) != 0 {
            wait_for_group_exit(child_pid, GROUP_DRAIN_TIMEOUT);
        }
        handle.close();
        let _ = forwarder.join();
        status
    })?;
    drop(signals);

    let forwarded = last_signal.load(Ordering::Acquire);
    if forwarded != 0 {
        return Ok(terminate_by_signal(forwarded));
    }
    Ok(exit_code_from_status(status))
}

fn exit_code_from_status(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    if let Some(sig) = status.signal() {
        return 128 + sig;
    }
    1
}

/// Re-raise `sig` with its OS default handler so the caller sees termination by
/// that signal (aborting shell loops on Ctrl-C). Returns `128 + sig` only as a
/// fallback if emulation somehow returns.
fn terminate_by_signal(sig: i32) -> i32 {
    let _ = signal_hook::low_level::emulate_default_handler(sig);
    128 + sig
}

fn signals_to_forward() -> Vec<i32> {
    let ignored = ignored_signal_mask();
    FORWARDED_SIGNALS
        .into_iter()
        .filter(|&sig| !is_signal_ignored(ignored, sig))
        .collect()
}

fn is_signal_ignored(mask: u64, sig: i32) -> bool {
    u32::try_from(sig)
        .ok()
        .and_then(|num| num.checked_sub(1))
        .filter(|&bit| bit < u64::BITS)
        .is_some_and(|bit| (mask & (1_u64 << bit)) != 0)
}

#[cfg(target_os = "linux")]
fn ignored_signal_mask() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| parse_sig_ign(&status))
        .unwrap_or(0)
}

/// Non-Linux hosts have no safe API in std, `rustix`, or `signal-hook` to read
/// an inherited signal disposition without replacing it, so all three signals
/// are forwarded there.
#[cfg(not(target_os = "linux"))]
fn ignored_signal_mask() -> u64 {
    0
}

#[cfg(any(target_os = "linux", test))]
fn parse_sig_ign(status: &str) -> Option<u64> {
    status.lines().find_map(|line| {
        let hex = line.strip_prefix("SigIgn:")?.trim();
        u64::from_str_radix(hex, 16).ok()
    })
}

fn forward_signal(child_pid: Pid, sig: i32, child_reaped: &AtomicBool) {
    let Some(signal) = to_rustix_signal(sig) else {
        return;
    };
    forward_signal_with(
        child_reaped,
        || kill_process_group(child_pid, signal),
        || {
            let _ = kill_process(child_pid, signal);
        },
    );
}

fn forward_signal_with(
    child_reaped: &AtomicBool,
    mut signal_group: impl FnMut() -> rustix::io::Result<()>,
    signal_pid: impl FnOnce(),
) {
    // Before the inner reaches `setsid`, process group `child_pid` does not
    // exist yet (`ESRCH`); fall back to signalling the inner pid directly while
    // it has not been reaped, then signal the group once more in case the inner
    // crossed `setsid` and spawned a child between the two calls.
    if let Err(rustix::io::Errno::SRCH) = signal_group()
        && !child_reaped.load(Ordering::Acquire)
    {
        signal_pid();
        let _ = signal_group();
    }
}

fn to_rustix_signal(sig: i32) -> Option<Signal> {
    match sig {
        SIGINT => Some(Signal::INT),
        SIGTERM => Some(Signal::TERM),
        SIGHUP => Some(Signal::HUP),
        _ => None,
    }
}

fn wait_for_group_exit(group_id: Pid, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while group_has_live_members(group_id) && Instant::now() < deadline {
        thread::sleep(GROUP_POLL_INTERVAL);
    }
}

#[cfg(target_os = "linux")]
fn group_has_live_members(group_id: Pid) -> bool {
    // `kill(-pgid, 0)` succeeds on Linux when the only remaining members of the
    // group are zombies waiting on a non-reaping adopter. Fast-path out once
    // the group is gone, otherwise check `/proc` for any non-zombie member.
    if test_kill_process_group(group_id).is_err() {
        return false;
    }
    let target_pgrp = group_id.as_raw_nonzero().get();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return true;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !name.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        if stat_is_live_group_member(&stat, target_pgrp) {
            return true;
        }
    }
    false
}

#[cfg(not(target_os = "linux"))]
fn group_has_live_members(group_id: Pid) -> bool {
    test_kill_process_group(group_id).is_ok()
}

#[cfg(target_os = "linux")]
fn stat_is_live_group_member(stat: &str, target_pgrp: i32) -> bool {
    let Some((_, after_comm)) = stat.rsplit_once(')') else {
        return false;
    };
    let mut fields = after_comm.split_whitespace();
    let Some(state) = fields.next() else {
        return false;
    };
    let _ppid = fields.next();
    let Some(pgrp) = fields.next().and_then(|value| value.parse::<i32>().ok()) else {
        return false;
    };
    pgrp == target_pgrp && !matches!(state, "Z" | "X")
}
