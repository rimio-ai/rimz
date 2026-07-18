//! Droid hook-emitter process classification.
//!
//! Droid 0.171.0's stock TUI starts an internal stream-JSON-RPC worker and
//! both processes inherit global hooks. The worker is the canonical emitter;
//! its observations stay owned by the outer TUI for pane liveness.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::Path;

const MAX_ANCESTOR_DEPTH: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HookProcessDisposition {
    StockTui,
    InternalWorker { owner_pid: u32 },
    Standalone { owner_pid: u32 },
}

pub(super) fn hook_process_disposition(pid: u32) -> HookProcessDisposition {
    hook_process_disposition_with(pid, &crate::proc::argv, &|pid| {
        crate::proc::comm_and_ppid(pid).map(|(_, ppid)| ppid)
    })
}

fn hook_process_disposition_with(
    pid: u32,
    argv: &dyn Fn(u32) -> Option<Vec<OsString>>,
    parent: &dyn Fn(u32) -> Option<u32>,
) -> HookProcessDisposition {
    let Some(args) = argv(pid) else {
        return HookProcessDisposition::Standalone { owner_pid: pid };
    };
    if !is_droid_argv(&args) {
        return HookProcessDisposition::Standalone { owner_pid: pid };
    }
    if !is_exec(&args) {
        return HookProcessDisposition::StockTui;
    }
    if !is_stream_jsonrpc_worker(&args) {
        return HookProcessDisposition::Standalone { owner_pid: pid };
    }

    let mut seen = HashSet::from([pid]);
    let mut current = pid;
    for _ in 0..MAX_ANCESTOR_DEPTH {
        let Some(ppid) = parent(current).filter(|ppid| *ppid > 1) else {
            return HookProcessDisposition::Standalone { owner_pid: pid };
        };
        if !seen.insert(ppid) {
            return HookProcessDisposition::Standalone { owner_pid: pid };
        }
        let Some(parent_args) = argv(ppid) else {
            return HookProcessDisposition::Standalone { owner_pid: pid };
        };
        if is_droid_argv(&parent_args) && !is_exec(&parent_args) {
            return HookProcessDisposition::InternalWorker { owner_pid: ppid };
        }
        current = ppid;
    }
    HookProcessDisposition::Standalone { owner_pid: pid }
}

fn is_droid_argv(args: &[OsString]) -> bool {
    args.first()
        .and_then(|program| Path::new(program).file_name())
        .and_then(OsStr::to_str)
        .is_some_and(|program| crate::agents::program_names_kind(program, "droid"))
}

fn is_exec(args: &[OsString]) -> bool {
    args.get(1).is_some_and(|arg| arg == "exec")
}

fn is_stream_jsonrpc_worker(args: &[OsString]) -> bool {
    is_exec(args)
        && has_option(args, "--input-format", "stream-jsonrpc")
        && has_option(args, "--output-format", "stream-jsonrpc")
}

fn has_option(args: &[OsString], option: &str, value: &str) -> bool {
    args.windows(2)
        .any(|pair| pair[0] == option && pair[1] == value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn classify(table: &[(u32, u32, &[&str])], pid: u32) -> HookProcessDisposition {
        let table = table
            .iter()
            .map(|(pid, ppid, args)| {
                (
                    *pid,
                    (
                        *ppid,
                        args.iter().map(OsString::from).collect::<Vec<OsString>>(),
                    ),
                )
            })
            .collect::<HashMap<_, _>>();
        hook_process_disposition_with(
            pid,
            &|pid| table.get(&pid).map(|(_, args)| args.clone()),
            &|pid| table.get(&pid).map(|(ppid, _)| *ppid),
        )
    }

    #[test]
    fn stock_tui_shapes_are_suppressed() {
        for args in [
            &["droid"][..],
            &["droid", "--auto", "medium"][..],
            &["droid", "--resume", "session-1"][..],
            &["droid", "--fork", "session-1"][..],
            &["droid", "fix", "the", "parser"][..],
            &["/opt/droid-aarch64-apple-darwin", "--use-spec"][..],
        ] {
            assert_eq!(
                classify(&[(10, 1, args)], 10),
                HookProcessDisposition::StockTui,
                "{args:?}"
            );
        }
    }

    #[test]
    fn internal_stream_worker_uses_outer_tui_owner() {
        let worker = &[
            "droid",
            "exec",
            "--input-format",
            "stream-jsonrpc",
            "--output-format",
            "stream-jsonrpc",
        ];
        let table = [
            (30, 20, worker.as_slice()),
            (20, 10, &["node", "wrapper.js"][..]),
            (10, 1, &["droid", "--resume", "session-1"][..]),
        ];
        assert_eq!(
            classify(&table, 30),
            HookProcessDisposition::InternalWorker { owner_pid: 10 }
        );
    }

    #[test]
    fn direct_exec_and_unrecognized_processes_remain_self_owned() {
        for args in [
            &["droid", "exec", "say hi"][..],
            &[
                "droid",
                "exec",
                "--input-format",
                "stream-jsonrpc",
                "--output-format",
                "stream-jsonrpc",
            ][..],
            &["sh", "-c", "droid exec --input-format stream-jsonrpc"][..],
        ] {
            assert_eq!(
                classify(&[(30, 1, args)], 30),
                HookProcessDisposition::Standalone { owner_pid: 30 },
                "{args:?}"
            );
        }
    }

    #[test]
    fn unreadable_bounded_and_cyclic_ancestry_fail_open() {
        let worker = &[
            "droid",
            "exec",
            "--input-format",
            "stream-jsonrpc",
            "--output-format",
            "stream-jsonrpc",
        ];
        assert_eq!(
            classify(&[(30, 20, worker.as_slice())], 30),
            HookProcessDisposition::Standalone { owner_pid: 30 }
        );
        assert_eq!(
            classify(&[(30, 20, worker.as_slice()), (20, 30, &["sh"][..])], 30),
            HookProcessDisposition::Standalone { owner_pid: 30 }
        );

        let mut deep = vec![(100, 99, worker.as_slice())];
        for pid in 67..=99 {
            deep.push((pid, pid - 1, &["sh"][..]));
        }
        deep.push((66, 1, &["droid"][..]));
        assert_eq!(
            classify(&deep, 100),
            HookProcessDisposition::Standalone { owner_pid: 100 }
        );
    }
}
