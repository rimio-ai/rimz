use super::*;

#[test]
fn resolve_candidate_root_handles_empty_unique_and_cwd_narrowing() {
    assert_eq!(
        resolve_candidate_root(&[], Some("/repo"), &|_| {
            panic!("empty candidates do not need cwd")
        }),
        None
    );

    assert_eq!(
        resolve_candidate_root(&[(300, 200), (301, 200)], None, &|_| {
            panic!("one root does not need cwd")
        }),
        Some(200)
    );

    let candidates = [(300, 200), (310, 210)];
    assert_eq!(
        resolve_candidate_root(&candidates, Some("/repo/feature"), &|pid| match pid {
            300 => Some(PathBuf::from("/repo/main")),
            310 => Some(PathBuf::from("/repo/feature")),
            _ => None,
        }),
        Some(210)
    );
}

#[test]
fn unique_foreground_match_backfills_the_pane_root() {
    // The htop pane: Zellij reports the foreground cmdline; the matcher
    // finds the one forest process with it and binds the pane to its root
    // (the direct server child, the zsh) — tmux's `#{pane_pid}` semantics,
    // so the shell→single-child descent then reads htop's stats.
    let procs = vec![
        server(100, SESSION),
        proc_info(200, 100, "zsh"),
        proc_info(300, 200, "htop"),
    ];
    let mut panes = vec![pane("terminal_4", Some("htop"), Some("/repo"))];
    backfill(&mut panes, &procs, &[]);
    assert_eq!(panes[0].pane_pid, Some(200));

    // An agent that chdir'd into its worktree sits in another directory
    // than its pane reports (`claude --worktree`), so a unique cmdline
    // match must bind without a cwd comparison.
    let procs = vec![
        server(100, SESSION),
        proc_info(200, 100, "zsh"),
        proc_info(300, 200, "claude --worktree feature"),
    ];
    let mut panes = vec![pane(
        "terminal_8",
        Some("claude --worktree feature"),
        Some("/repo"),
    )];
    backfill(&mut panes, &procs, &[(300, "/repo/worktrees/feature")]);
    assert_eq!(panes[0].pane_pid, Some(200));
}

#[test]
fn cwd_narrows_same_command_candidates() {
    // Two panes both run `htop`, one per worktree: the cmdline ties, the
    // foreground's `/proc` cwd breaks it, and each pane binds its own root.
    let procs = vec![
        server(100, SESSION),
        proc_info(200, 100, "zsh"),
        proc_info(300, 200, "htop"),
        proc_info(210, 100, "zsh"),
        proc_info(310, 210, "htop"),
    ];
    let mut panes = vec![
        pane("terminal_1", Some("htop"), Some("/wt1")),
        pane("terminal_2", Some("htop"), Some("/wt2")),
    ];
    backfill(&mut panes, &procs, &[(300, "/wt1"), (310, "/wt2")]);
    assert_eq!(panes[0].pane_pid, Some(200));
    assert_eq!(panes[1].pane_pid, Some(210));
}

#[test]
fn claimed_roots_are_eliminated_from_other_pane_candidates() {
    // One pane's root pid restored from cache leaves the same-cmdline same-cwd
    // sibling with the remaining root instead of forcing both to abstain.
    let procs = vec![
        server(100, SESSION),
        proc_info(200, 100, "zsh"),
        proc_info(300, 200, "codex"),
        proc_info(210, 100, "zsh"),
        proc_info(310, 210, "codex"),
    ];
    let mut known = pane("terminal_1", Some("codex"), Some("/repo"));
    known.pane_pid = Some(200);
    let mut panes = vec![known, pane("terminal_2", Some("codex"), Some("/repo"))];
    backfill(&mut panes, &procs, &[(300, "/repo"), (310, "/repo")]);

    assert_eq!(panes[0].pane_pid, Some(200));
    assert_eq!(panes[1].pane_pid, Some(210));
}

#[test]
fn ambiguous_candidates_abstain() {
    // Two idle `zsh` panes in one cwd are indistinguishable — by cmdline
    // and by cwd — so both stay pidless: no stats beats a stranger's stats.
    let procs = vec![
        server(100, SESSION),
        proc_info(200, 100, "zsh"),
        proc_info(210, 100, "zsh"),
    ];
    let mut panes = vec![
        pane("terminal_6", Some("zsh"), Some("/repo")),
        pane("terminal_14", Some("zsh"), Some("/repo")),
    ];
    backfill(&mut panes, &procs, &[(200, "/repo"), (210, "/repo")]);
    assert_eq!(panes[0].pane_pid, None);
    assert_eq!(panes[1].pane_pid, None);
}

#[test]
fn deep_foreground_walks_up_to_the_server_child() {
    // A launcher chain (zsh → npm → node script): the foreground match is
    // levels deep, and the walk still lands on the direct server child. A
    // foreground that *is* the server child binds itself.
    let procs = vec![
        server(100, SESSION),
        proc_info(200, 100, "zsh"),
        proc_info(300, 200, "npm run build"),
        proc_info(400, 300, "node /repo/build.js"),
        proc_info(500, 100, "claude remote-control --spawn worktree"),
    ];
    let mut panes = vec![
        pane("terminal_3", Some("node /repo/build.js"), Some("/repo")),
        pane(
            "terminal_1",
            Some("claude remote-control --spawn worktree"),
            Some("/repo"),
        ),
    ];
    backfill(&mut panes, &procs, &[]);
    assert_eq!(panes[0].pane_pid, Some(200));
    assert_eq!(panes[1].pane_pid, Some(500));
}

#[test]
fn no_matching_server_is_a_no_op() {
    // Another session's server, another uid's same-named server, or no uid
    // at all (non-Linux): the backfill leaves every pane untouched rather
    // than walking a stranger's forest.
    let mut other_uid = server(100, SESSION);
    other_uid.real_uid = 1001;
    let procs = vec![
        server(110, "rimz-other"),
        other_uid,
        proc_info(200, 100, "zsh"),
        proc_info(300, 200, "htop"),
    ];
    let mut panes = vec![pane("terminal_4", Some("htop"), Some("/repo"))];
    backfill(&mut panes, &procs, &[]);
    assert_eq!(panes[0].pane_pid, None);

    let procs_ok = vec![server(100, SESSION), proc_info(300, 100, "htop")];
    let mut frame = frame_from_panes(panes.clone());
    backfill_zellij_pane_pids(
        &mut frame,
        &procs_ok,
        &children_of(&procs_ok),
        SESSION,
        None, // unknown own uid: skip rather than guess
        &|_| None,
    );
    sync_panes_from_frame(&mut panes, frame);
    assert_eq!(panes[0].pane_pid, None);
}

#[test]
fn chrome_and_already_pidded_panes_are_left_alone() {
    // Sidebar chrome shares one cmdline across panes and is excluded from
    // rows, so it is skipped outright; a pane the backend already pidded
    // (tmux) is never re-derived.
    let procs = vec![
        server(100, SESSION),
        proc_info(200, 100, "rimz-sidebar"),
        proc_info(210, 100, "zsh"),
        proc_info(300, 210, "htop"),
    ];
    let chrome = pane("terminal_0", Some("rimz-sidebar"), Some("/repo"));
    let mut pidded = pane("terminal_4", Some("htop"), Some("/repo"));
    pidded.pane_pid = Some(42);
    let mut panes = vec![chrome, pidded];
    backfill(&mut panes, &procs, &[]);
    assert_eq!(panes[0].pane_pid, None);
    assert_eq!(panes[1].pane_pid, Some(42));
}
