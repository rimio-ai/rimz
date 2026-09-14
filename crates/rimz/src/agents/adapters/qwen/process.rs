//! Qwen hook-emitter process ownership.
//!
//! Qwen Code 0.23 runs `StopFailure`, `MessageDisplay`, and `SessionDelete`
//! command hooks under a detached `node --input-type=commonjs --eval`
//! supervisor so they survive Qwen's exit. The hook shell's `$PPID` is that
//! short-lived supervisor, which must never become the session's runtime owner.

use std::ffi::{OsStr, OsString};
use std::path::Path;

/// Resolve the process that owns a hook emitted with `RIMZ_AGENT_PID=$PPID`.
/// A surviving-hook supervisor resolves to its live Qwen parent, or abstains
/// once Qwen has exited and the supervisor was reparented.
pub(super) fn hook_owner_pid(pid: u32) -> Option<u32> {
    hook_owner_pid_with(pid, &crate::proc::argv, &|pid| {
        crate::proc::comm_and_ppid(pid).map(|(_, ppid)| ppid)
    })
}

fn hook_owner_pid_with(
    pid: u32,
    argv: &dyn Fn(u32) -> Option<Vec<OsString>>,
    parent: &dyn Fn(u32) -> Option<u32>,
) -> Option<u32> {
    if !argv(pid).is_some_and(|args| is_surviving_hook_supervisor(&args)) {
        return Some(pid);
    }
    let ppid = parent(pid).filter(|ppid| *ppid > 1)?;
    argv(ppid)
        .is_some_and(|args| runs_as_qwen(&args) && !is_surviving_hook_supervisor(&args))
        .then_some(ppid)
}

fn is_surviving_hook_supervisor(args: &[OsString]) -> bool {
    runs_as_qwen(args)
        && args
            .get(1)
            .is_some_and(|arg| arg == "--input-type=commonjs")
        && args.get(2).is_some_and(|arg| arg == "--eval")
}

fn runs_as_qwen(args: &[OsString]) -> bool {
    args.first()
        .and_then(|program| Path::new(program).file_name())
        .and_then(OsStr::to_str)
        .is_some_and(|program| super::QWEN_DESCRIPTOR.runs_as(program))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const QWEN: &[&str] = &[
        "/opt/qwen-code/node/bin/node",
        "--expose-gc",
        "/opt/qwen-code/lib/cli.js",
    ];
    const SUPERVISOR: &[&str] = &[
        "/opt/qwen-code/node/bin/node",
        "--input-type=commonjs",
        "--eval",
        "'use strict';",
        "/tmp/qwen-hook-input.json",
        "10000",
    ];

    fn owner(table: &[(u32, u32, &[&str])], pid: u32) -> Option<u32> {
        let table = table
            .iter()
            .map(|(pid, ppid, args)| (*pid, (*ppid, args.iter().map(OsString::from).collect())))
            .collect::<HashMap<u32, (u32, Vec<OsString>)>>();
        hook_owner_pid_with(
            pid,
            &|pid| table.get(&pid).map(|(_, args)| args.clone()),
            &|pid| table.get(&pid).map(|(ppid, _)| *ppid),
        )
    }

    #[test]
    fn direct_hooks_keep_the_qwen_pid() {
        assert_eq!(owner(&[(10, 5, QWEN)], 10), Some(10));
        assert_eq!(owner(&[], 10), Some(10));
    }

    #[test]
    fn surviving_hook_supervisor_resolves_to_its_live_qwen_parent() {
        assert_eq!(owner(&[(10, 5, QWEN), (20, 10, SUPERVISOR)], 20), Some(10));
    }

    #[test]
    fn orphaned_or_unverified_supervisor_abstains() {
        assert_eq!(owner(&[(20, 1, SUPERVISOR)], 20), None);
        assert_eq!(owner(&[(20, 30, SUPERVISOR)], 20), None);
        assert_eq!(owner(&[(30, 5, &["bash"]), (20, 30, SUPERVISOR)], 20), None);
    }

    #[test]
    fn eval_arguments_of_other_programs_are_not_supervisors() {
        assert_eq!(
            owner(
                &[(20, 10, &["python3", "--input-type=commonjs", "--eval"])],
                20
            ),
            Some(20)
        );
    }
}
