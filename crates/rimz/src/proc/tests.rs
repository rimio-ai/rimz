use super::*;

#[cfg(target_os = "linux")]
#[test]
fn host_proc_counters_measure_open_attempts_including_composite_readers() {
    use crate::testkit::host_pass::{Counter::*, thread_counters};
    let counts = thread_counters();
    let kinds = [
        ProcStat,
        ProcCmdline,
        ProcComm,
        ProcChildren,
        ProcTaskDir,
        ProcCwd,
        ProcIo,
        ProcStatus,
        ProcBootStat,
    ];
    type ProcProbe = fn(u32) -> bool;
    let probes: [(ProcProbe, [u64; 9]); 12] = [
        (|pid| comm(pid).is_some(), [0, 0, 1, 0, 0, 0, 0, 0, 0]),
        (|pid| cmdline(pid).is_some(), [0, 1, 0, 0, 0, 0, 0, 0, 0]),
        (|pid| argv(pid).is_some(), [0, 1, 0, 0, 0, 0, 0, 0, 0]),
        (|pid| cwd(pid).is_some(), [0, 0, 0, 0, 0, 1, 0, 0, 0]),
        (|pid| io_bytes(pid).is_some(), [0, 0, 0, 0, 0, 0, 1, 0, 0]),
        (
            |pid| write_bytes(pid).is_some(),
            [0, 0, 0, 0, 0, 0, 1, 0, 0],
        ),
        (
            |pid| comm_and_ppid(pid).is_some(),
            [0, 0, 0, 0, 0, 0, 0, 1, 0],
        ),
        (|pid| real_uid(pid).is_some(), [0, 0, 0, 0, 0, 0, 0, 1, 0]),
        (
            |pid| stat_metrics(pid).is_some(),
            [1, 0, 0, 0, 0, 0, 0, 0, 0],
        ),
        (
            |pid| process_start_token(pid).is_some(),
            [1, 0, 0, 0, 0, 0, 0, 0, 0],
        ),
        (
            |pid| process_start(pid).is_some(),
            [1, 0, 0, 0, 0, 0, 0, 0, 1],
        ),
        (|pid| read_proc(pid).is_some(), [0, 1, 0, 0, 0, 0, 0, 1, 0]),
    ];
    for (probe, expected) in probes {
        counts.reset();
        assert!(probe(std::process::id()));
        assert_eq!(kinds.map(|kind| counts.get(kind)), expected);
    }
    counts.reset();
    children(std::process::id());
    assert!(counts.get(ProcChildren) > 0);
    assert_eq!(counts.get(ProcTaskDir), 1);
    assert_eq!(counts.get(ProcStat), 0);
    counts.reset();
    process_start(u32::MAX);
    assert_eq!(
        kinds.map(|kind| counts.get(kind)),
        [1, 0, 0, 0, 0, 0, 0, 0, 0]
    );
    counts.reset();
    parse_children("1 2");
    assert_eq!(kinds.map(|kind| counts.get(kind)), [0; 9]);
}

#[cfg(unix)]
#[test]
fn path_copies_collapse_same_files_exclude_the_hit_and_keep_path_order() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = tempfile::tempdir().unwrap();
    let dirs =
        ["first", "linked", "hardlinked", "second", "third"].map(|name| root.path().join(name));
    for dir in [&dirs[0], &dirs[3], &dirs[4]] {
        std::fs::create_dir(dir).unwrap();
        let shell = dir.join("bash");
        std::fs::write(&shell, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(shell, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    symlink(&dirs[0], &dirs[1]).unwrap();
    std::fs::create_dir(&dirs[2]).unwrap();
    std::fs::hard_link(dirs[0].join("bash"), dirs[2].join("bash")).unwrap();
    let path = std::env::join_paths(&dirs).unwrap();
    let copies = [None, Some(dirs[0].join("bash")), Some(dirs[2].join("bash"))].map(|exclude| {
        path_copies_with(
            std::ffi::OsStr::new("bash"),
            Some(&path),
            Some(root.path()),
            exclude.as_deref(),
        )
    });
    let expected = |indices: &[usize]| {
        indices
            .iter()
            .map(|index| {
                let path = dirs[*index].join("bash");
                (path.clone(), path.canonicalize().unwrap())
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        copies,
        [expected(&[0, 3, 4]), expected(&[3, 4]), expected(&[3, 4])]
    );
}

#[cfg(target_os = "linux")]
#[test]
fn input_tty_path_decodes_pts_and_rejects_non_pts() {
    let stat = |major, minor| {
        let device = u32::try_from(nix::sys::stat::makedev(major, minor)).unwrap();
        format!(
            "123 (provider (worker)) S 1 2 3 {} -1 0",
            i32::from_ne_bytes(device.to_ne_bytes())
        )
    };
    for (major, minor, index) in [
        (136, 42, 42),
        (137, 7, 263),
        (143, 255, 2047),
        (136, 600_000, 600_000),
    ] {
        assert_eq!(
            input_tty_path(&stat(major, minor)),
            Some(PathBuf::from(format!("/dev/pts/{index}")))
        );
    }
    for line in [
        stat(0, 0),
        stat(4, 1),
        stat(128, 1),
        stat(144, 0),
        "bad stat".into(),
        "1 (sh) S 1 2 3 nope".into(),
    ] {
        assert_eq!(input_tty_path(&line), None);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn tree_peak_reads_high_water_and_rejects_missing_key() {
    assert_eq!(
        super::parse_tree_memory("VmHWM:\t    1800 kB\nVmSwap: 400 kB\n"),
        Some(super::TreeMemory {
            peak_rss_kb: 1800,
            swap_kb: 400
        })
    );
    assert_eq!(
        super::parse_tree_memory("VmHWM: 100 kB\n"),
        Some(super::TreeMemory {
            peak_rss_kb: 100,
            swap_kb: 0
        })
    );
    assert_eq!(
        super::parse_tree_memory("VmRSS: 100 kB\nVmSwap: 20 kB\n"),
        None
    );
}

#[cfg(target_os = "linux")]
#[test]
fn tree_peak_sums_descendants_and_skips_unreadable_children() {
    let children = |pid| match pid {
        1 => vec![2, 3],
        2 => vec![1, 4],
        _ => vec![],
    };
    let memory = |pid| {
        let status = match pid {
            1 => "VmHWM: 100 kB\nVmSwap: 20 kB\n",
            4 => "VmHWM: 300 kB\nVmSwap: 40 kB\n",
            _ => "VmSwap: 1000 kB\n",
        };
        super::parse_tree_memory(status)
    };
    assert_eq!(
        super::tree_memory_kb_with(1, &memory, &children),
        Some(super::TreeMemory {
            peak_rss_kb: 400,
            swap_kb: 60
        })
    );
    assert_eq!(super::tree_memory_kb_with(2, &memory, &children), None);
}

#[test]
fn resolve_existing_or_replacement_strips_deleted_suffix_when_replacement_exists() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("rimz");
    std::fs::write(&real, b"x").unwrap();
    let deleted = PathBuf::from(format!("{} (deleted)", real.display()));

    assert_eq!(
        resolve_existing_or_replacement(&deleted),
        Some(real.clone())
    );
    assert_eq!(resolve_existing_or_replacement(&real), Some(real));
}

#[test]
fn resolve_existing_or_replacement_returns_none_when_no_file_exists() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("rimz");
    let deleted = PathBuf::from(format!("{} (deleted)", missing.display()));

    assert_eq!(resolve_existing_or_replacement(&deleted), None);
    assert_eq!(resolve_existing_or_replacement(&missing), None);
}

#[test]
fn launchable_shell_rejects_missing_and_disabled_shells() {
    assert!(!launchable_shell(Path::new("/definitely/not/a/shell")));
    assert!(!launchable_shell(Path::new("/usr/sbin/nologin")));
    assert!(!launchable_shell(Path::new("/bin/false")));
}

#[test]
fn user_shell_prefers_stamped_user_shell_over_agent_shell() {
    let dir = tempfile::tempdir().unwrap();
    let user = dir.path().join("zsh");
    let agent = dir.path().join("bash");
    std::fs::write(&user, b"").unwrap();
    std::fs::write(&agent, b"").unwrap();

    assert_eq!(
        user_shell_from(Some(user.clone()), Some(agent.clone())),
        Some(user)
    );
    assert_eq!(
        user_shell_from(None, Some(agent.clone())),
        Some(agent.clone())
    );
    assert_eq!(
        user_shell_from(Some(dir.path().join("missing")), Some(agent)),
        None
    );
}

#[test]
fn shell_pane_name_uses_configured_shell_basename() {
    let shell = user_shell_program();
    let expected = Path::new(&shell)
        .file_name()
        .and_then(|name| name.to_str())
        .expect("shell basename");
    assert_eq!(shell_pane_name(), expected);
}

#[cfg(target_os = "linux")]
mod procfs {
    use super::super::*;

    #[test]
    fn output_tail_caps_bytes_and_preserves_lossy_boundary_behavior() {
        assert_eq!(tail_output(b"abcdef", 4), "cdef");
        assert_eq!(tail_output("éabc".as_bytes(), 4), "�abc");
        assert_eq!(tail_output(b"abc", 0), "");
        assert_eq!(tail_output(b"abc", 8), "abc");
    }

    #[test]
    fn parse_comm_trims_trailing_newline() {
        assert_eq!(parse_comm("zsh\n").as_deref(), Some("zsh"));
        assert_eq!(parse_comm("bash").as_deref(), Some("bash"));
    }

    #[test]
    fn parse_comm_rejects_blank() {
        assert_eq!(parse_comm("\n"), None);
        assert_eq!(parse_comm("   "), None);
    }

    #[test]
    fn parse_environ_reads_a_present_key() {
        let blob = b"PATH=/usr/bin\0ZELLIJ_PANE_ID=3\0TERM=xterm\0";
        assert_eq!(parse_environ(blob, "ZELLIJ_PANE_ID").as_deref(), Some("3"));
        assert_eq!(parse_environ(blob, "PATH").as_deref(), Some("/usr/bin"));
    }

    #[test]
    fn parse_environ_missing_key_is_none() {
        let blob = b"PATH=/usr/bin\0TERM=xterm\0";
        assert_eq!(parse_environ(blob, "ZELLIJ_PANE_ID"), None);
        // A bare key with no `=` never matches the `key=` prefix.
        assert_eq!(parse_environ(b"ZELLIJ_PANE_ID\0", "ZELLIJ_PANE_ID"), None);
    }

    #[test]
    fn parse_environ_pairs_preserves_empty_and_equals_values() {
        assert_eq!(
            parse_environ_pairs(b"EMPTY=\0TOKEN=left=right\0BARE\0"),
            vec![
                ("EMPTY".to_owned(), String::new()),
                ("TOKEN".to_owned(), "left=right".to_owned()),
            ]
        );
    }

    #[test]
    fn rimz_exe_resolution_keeps_existing_path() {
        let path = Path::new("/opt/rimz/bin/rimz");
        let resolved = resolve_existing_or_replacement_with(path, |candidate| candidate == path);

        assert_eq!(resolved.as_deref(), Some(path));
    }

    #[test]
    fn rimz_exe_resolution_strips_deleted_suffix_when_replacement_exists() {
        let real = Path::new("/opt/rimz/bin/rimz");
        let deleted = PathBuf::from(format!("{} (deleted)", real.display()));
        let resolved =
            resolve_existing_or_replacement_with(&deleted, |candidate| candidate == real);

        assert_eq!(resolved.as_deref(), Some(real));
    }

    #[test]
    fn rimz_exe_resolution_returns_none_when_deleted_and_replacement_missing() {
        let deleted = Path::new("/opt/rimz/bin/rimz (deleted)");

        assert!(resolve_existing_or_replacement_with(deleted, |_| false).is_none());
    }

    #[test]
    fn parse_status_identity_reads_ppid_and_real_uid() {
        let status = "\
Name:\tclaude
State:\tS (sleeping)
PPid:\t42
Uid:\t0\t0\t0\t0
";
        assert_eq!(parse_status_identity(status), Some((42, 0)));
    }

    #[test]
    fn parse_argv_preserves_argument_boundaries() {
        use std::ffi::OsStr;

        let args = parse_argv(b"droid\0exec\0--input-format\0stream-jsonrpc\0").unwrap();
        assert_eq!(
            args.iter()
                .map(std::ffi::OsString::as_os_str)
                .collect::<Vec<_>>(),
            [
                OsStr::new("droid"),
                OsStr::new("exec"),
                OsStr::new("--input-format"),
                OsStr::new("stream-jsonrpc"),
            ]
        );
        assert_eq!(parse_argv(b""), None);
    }

    #[test]
    fn parse_status_name_ppid_reads_name_and_parent() {
        let status = "\
Name:\tcodex
State:\tS (sleeping)
PPid:\t42
Uid:\t1000\t1000\t1000\t1000
";
        assert_eq!(
            parse_status_name_ppid(status),
            Some(("codex".to_owned(), 42))
        );
    }

    #[test]
    fn parse_status_identity_rejects_missing_fields() {
        assert_eq!(parse_status_identity("PPid:\t42\n"), None);
        assert_eq!(
            parse_status_identity("Uid:\t1000\t1000\t1000\t1000\n"),
            None
        );
    }

    #[test]
    fn parse_starttime_ticks_reads_field_22() {
        // pid (comm) state ppid … starttime(field 22) …
        let stat = "1234 (codex) S 1 1234 1234 0 -1 0 0 0 0 0 1 2 3 4 20 0 5 0 646245020 …";
        assert_eq!(parse_starttime_ticks(stat), Some(646_245_020));
    }

    #[test]
    fn parse_starttime_ticks_handles_comm_with_spaces_and_parens() {
        // `comm` can carry spaces and nested parens; anchoring on the last `)`
        // keeps field 22 correct.
        let stat = "1234 (codex (1) :)) S 1 1234 1234 0 -1 0 0 0 0 0 1 2 3 4 20 0 5 0 777 0";
        assert_eq!(parse_starttime_ticks(stat), Some(777));
    }

    #[test]
    fn parse_starttime_ticks_rejects_malformed() {
        assert_eq!(parse_starttime_ticks("no parens here"), None);
        // Too few fields after `comm` to reach field 22.
        assert_eq!(parse_starttime_ticks("1 (sh) S 1 2 3"), None);
    }

    #[test]
    fn parse_stat_metrics_reads_cpu_rss_and_start_from_one_line() {
        // Indexed past the closing `)`:
        // state ppid pgrp session ttyno tpgid flags minflt cminflt majflt cmajflt utime stime
        //   0    1    2    3       4     5     6     7      8       9      10      11    12
        // cutime cstime priority nice threads itreal starttime vsize rss
        //   13     14      15     16    17      18      19       20   21
        let stat =
            "1234 (codex) S 1 1234 1234 0 -1 0 0 0 0 0 42 17 0 0 20 0 1 0 646245020 9000000 2048 …";
        assert_eq!(
            parse_stat_metrics(stat, 4),
            Some(StatMetrics {
                state: 'S',
                cpu_ticks: 59, // 42 + 17
                child_cpu_ticks: 0,
                rss_kb: 8192, // 2048 pages × 4 KiB
                start_ticks: 646_245_020,
            })
        );
    }

    #[test]
    fn parse_stat_metrics_handles_comm_with_spaces_and_parens() {
        // `comm` can carry spaces and nested parens; anchoring on the last `)`
        // keeps every field index correct.
        let stat = "1 (rust (1) :)) S 1 1 1 0 -1 0 0 0 0 0 10 5 0 0 20 0 1 0 100 500 3 0";
        let metrics = parse_stat_metrics(stat, 4).expect("stat metrics");
        assert_eq!(
            metrics,
            StatMetrics {
                state: 'S',
                cpu_ticks: 15, // 10 + 5
                child_cpu_ticks: 0,
                rss_kb: 12, // 3 pages × 4 KiB
                start_ticks: 100,
            }
        );
        assert!(stat_metrics_is_live(metrics, Some("100")));
        assert!(!stat_metrics_is_live(metrics, Some("101")));
    }

    #[test]
    fn process_start_liveness_rejects_zombie_and_dead_states() {
        let stat = |state| {
            format!("1 (rust (1) :)) {state} 1 1 1 0 -1 0 0 0 0 0 10 5 0 0 20 0 1 0 100 500 3 0")
        };

        let running = parse_stat_metrics(&stat('S'), 4).expect("running stat");
        let zombie = parse_stat_metrics(&stat('Z'), 4).expect("zombie stat");
        let dead = parse_stat_metrics(&stat('X'), 4).expect("dead stat");
        assert!(stat_metrics_is_live(running, None));
        let frozen = parse_stat_metrics(&stat('D'), 4).expect("frozen stat");
        assert!(stat_metrics_is_live(frozen, Some("100")));
        assert!(!stat_metrics_is_live(zombie, Some("100")));
        assert!(!stat_metrics_is_live(dead, Some("100")));
    }

    #[test]
    fn parse_stat_metrics_reads_waited_child_cpu() {
        let stat = "1234 (cargo) S 1 1234 1234 0 -1 0 0 0 0 0 42 17 300 25 20 0 1 0 646245020 9000000 2048 …";

        assert_eq!(
            parse_stat_metrics(stat, 4).map(|metrics| metrics.child_cpu_ticks),
            Some(325)
        );
    }

    #[test]
    fn parse_stat_metrics_rejects_truncated_lines() {
        // Enough fields for CPU but not for rss: the whole read abstains
        // rather than reporting a partial metric set.
        let stat = "1 (sh) S 1 1 1 0 -1 0 0 0 0 0 10 5 0 0 20 0 1 0 100";
        assert_eq!(parse_stat_metrics(stat, 4), None);
        assert_eq!(parse_stat_metrics("no parens here", 4), None);
    }

    #[test]
    fn parse_children_reads_the_space_separated_pid_list() {
        assert_eq!(parse_children("123 456 789 "), vec![123, 456, 789]);
        assert_eq!(parse_children(""), Vec::<u32>::new());
        // Garbage tokens are skipped rather than poisoning the list.
        assert_eq!(parse_children("12 x 34"), vec![12, 34]);
    }

    #[test]
    fn parse_io_bytes_sums_rchar_and_wchar() {
        let io = "rchar: 1000\nwchar: 500\nsyscr: 12\nsyscw: 8\nread_bytes: 0\nwrite_bytes: 512\n";
        assert_eq!(parse_io_bytes(io), Some(1500));
    }

    #[test]
    fn parse_write_bytes_reads_wchar_only() {
        let io =
            "rchar: 1000\nwchar: 500\nsyscr: 12\nsyscw: 9000\nread_bytes: 0\nwrite_bytes: 8192\n";
        assert_eq!(parse_write_bytes(io), Some(500));
    }

    #[test]
    fn parse_io_bytes_rejects_incomplete() {
        // Only rchar present: returns None (wchar is missing).
        let io = "rchar: 1000\n";
        assert_eq!(parse_io_bytes(io), None);
    }

    #[test]
    fn tree_totals_walks_descendants_and_dedupes_cycles() {
        let stat = |pid| {
            Some(StatMetrics {
                state: 'S',
                cpu_ticks: u64::from(pid),
                child_cpu_ticks: 10,
                rss_kb: u64::from(pid * 100),
                start_ticks: 1,
            })
        };
        let children = |pid| match pid {
            1 => vec![2, 3],
            2 => vec![3],
            _ => Vec::new(),
        };
        let io = |pid| Some(u64::from(pid * 1_000));

        assert_eq!(
            tree_totals_with(1, &stat, &children, &io),
            Some(TreeTotals {
                cpu_ticks: 36,
                rss_kb: 600,
                io_bytes: Some(6_000),
                process_count: 3,
            })
        );
    }

    #[test]
    fn tree_totals_returns_none_when_root_is_missing() {
        let stat = |_pid| None;
        let children = |_pid| Vec::new();
        let io = |_pid| Some(0);

        assert_eq!(tree_totals_with(1, &stat, &children, &io), None);
    }

    #[test]
    fn tree_totals_missing_child_io_makes_tree_io_unknown() {
        let stat = |pid| {
            Some(StatMetrics {
                state: 'S',
                cpu_ticks: u64::from(pid),
                child_cpu_ticks: 0,
                rss_kb: 1,
                start_ticks: 1,
            })
        };
        let children = |pid| if pid == 1 { vec![2] } else { Vec::new() };
        let io = |pid| (pid == 1).then_some(10);

        assert_eq!(
            tree_totals_with(1, &stat, &children, &io).map(|totals| totals.io_bytes),
            Some(None)
        );
    }

    #[test]
    fn root_path_identity_reads_a_path_through_the_process_root() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile::tempdir().unwrap();
        let metadata = std::fs::metadata(dir.path()).unwrap();
        let me = std::process::id();
        assert_eq!(
            root_path_identity(me, dir.path()),
            Some((metadata.dev(), metadata.ino()))
        );
        assert_eq!(root_path_identity(me, &dir.path().join("absent")), None);
        assert_eq!(root_path_identity(me, Path::new("relative")), None);
        assert_eq!(root_path_identity(u32::MAX, dir.path()), None);
    }

    #[test]
    fn parse_btime_reads_the_btime_line() {
        let stat = "cpu  1 2 3\nintr 99\nbtime 1773993132\nprocesses 42\n";
        assert_eq!(parse_btime(stat), Some(1_773_993_132));
        assert_eq!(parse_btime("cpu 1 2 3\nprocesses 42\n"), None);
    }
}
