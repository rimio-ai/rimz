use super::*;

/// A pane with the given id, command, and cwd; other fields are irrelevant
/// to the carry-forward logic under test.
fn pane(id: &str, command: Option<&str>, cwd: Option<&str>) -> rimz::feed::PaneRef {
    rimz::feed::PaneRef {
        pane_id: rimz::ids::PaneId::from_parts(MuxName::Zellij, id),
        session_name: "s".to_owned(),
        view_id: None,
        view_kind: None,
        view_name: None,
        is_focused: false,
        command: command.map(ToOwned::to_owned),
        cwd: cwd.map(ToOwned::to_owned),
        pane_pid: None,
        pane_process_start: None,
        rss_kb: None,
        cpu_pct: None,
        io_bps: None,
    }
}

#[test]
fn pane_fields_carry_forward_by_pane_id() {
    // A degraded read drops command and cwd; the last good read of the same
    // pane id backfills them so the row keeps its agent label and worktree
    // group instead of flashing a bare `process` under `external`.
    let mut fresh = vec![pane("terminal_1", None, None)];
    let prev = vec![pane("terminal_1", Some("claude"), Some("/repo"))];
    carry_forward_pane_fields(&mut fresh, &prev);
    assert_eq!(fresh[0].command.as_deref(), Some("claude"));
    assert_eq!(fresh[0].cwd.as_deref(), Some("/repo"));
}

#[test]
fn carry_forward_does_not_cross_pane_id() {
    // A different (e.g. reused) pane id reports its own fresh fields and is
    // never backfilled from a stranger's last-good read.
    let mut fresh = vec![pane("terminal_2", None, None)];
    let prev = vec![pane("terminal_1", Some("claude"), Some("/repo"))];
    carry_forward_pane_fields(&mut fresh, &prev);
    assert_eq!(fresh[0].command, None);
    assert_eq!(fresh[0].cwd, None);
}

#[test]
fn fresh_pane_field_wins_when_present() {
    // A genuine handoff (claude → zsh) is a real fresh value, not a dropped
    // field, so it is never overwritten by the prior tenant's command.
    let mut fresh = vec![pane("terminal_1", Some("zsh"), Some("/now"))];
    let prev = vec![pane("terminal_1", Some("claude"), Some("/repo"))];
    carry_forward_pane_fields(&mut fresh, &prev);
    assert_eq!(fresh[0].command.as_deref(), Some("zsh"));
    assert_eq!(fresh[0].cwd.as_deref(), Some("/now"));
}

#[test]
fn carry_forward_from_cache_backfills_from_disk() {
    // The shared repair both produce arms run: a raced read's dropped
    // fields backfill from the on-disk pane cache, so the wedged-producer
    // fallback path renders no anonymous row either.
    let dir = tempfile::tempdir().unwrap();
    let cache_path = dir.path().join("snapshot.json");
    let prior = SnapshotCache {
        produced_at_ms: 1,
        session_name: "s".to_owned(),
        panes: vec![pane("terminal_1", Some("claude"), Some("/repo"))],
    };
    atomic::write_temp_then_rename_cache(&cache_path, &prior).unwrap();
    let mut panes = vec![pane("terminal_1", None, None)];
    carry_forward_from_cache(&mut panes, &cache_path, "s");
    assert_eq!(panes[0].command.as_deref(), Some("claude"));
    assert_eq!(panes[0].cwd.as_deref(), Some("/repo"));
}

#[test]
fn carry_forward_from_cache_is_noop_without_prior() {
    // No cache on disk (the first tick after session birth): the read
    // passes through untouched rather than erroring.
    let dir = tempfile::tempdir().unwrap();
    let cache_path = dir.path().join("snapshot.json");
    let mut panes = vec![pane("terminal_1", None, None)];
    carry_forward_from_cache(&mut panes, &cache_path, "s");
    assert_eq!(panes[0].command, None);
    assert_eq!(panes[0].cwd, None);
}

/// A process-table entry for the pid-backfill matcher fixtures; everything
/// runs as one uid (1000) unless a test says otherwise.
fn proc_info(pid: u32, ppid: u32, cmdline: &str) -> rimz::proc::ProcInfo {
    rimz::proc::ProcInfo {
        pid,
        ppid,
        real_uid: 1000,
        cmdline: cmdline.to_owned(),
    }
}

/// The ppid→children map `enrich_pane_metrics` builds, over a fixture table.
fn children_of(procs: &[rimz::proc::ProcInfo]) -> HashMap<u32, Vec<u32>> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in procs {
        children.entry(p.ppid).or_default().push(p.pid);
    }
    children
}

/// The session's Zellij server process, socket named after the session.
fn server(pid: u32, session: &str) -> rimz::proc::ProcInfo {
    proc_info(
        pid,
        1,
        &format!("/usr/bin/zellij --server /run/user/1000/zellij/contract_version_1/{session}"),
    )
}

const SESSION: &str = "rimz-query-engine";

fn backfill(
    panes: &mut [rimz::feed::PaneRef],
    procs: &[rimz::proc::ProcInfo],
    cwds: &[(u32, &str)],
) {
    let cwds: HashMap<u32, PathBuf> = cwds
        .iter()
        .map(|(pid, cwd)| (*pid, PathBuf::from(cwd)))
        .collect();
    backfill_zellij_pane_pids(
        panes,
        procs,
        &children_of(procs),
        SESSION,
        Some(1000),
        &|pid| cwds.get(&pid).cloned(),
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
}

#[test]
fn unique_match_skips_the_cwd_check() {
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
    backfill_zellij_pane_pids(
        &mut panes,
        &procs_ok,
        &children_of(&procs_ok),
        SESSION,
        None, // unknown own uid: skip rather than guess
        &|_| None,
    );
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

#[test]
fn parse_numstat_sums_text_diff_and_ignores_binary_rows() {
    let stats = parse_numstat("12\t4\tsrc/lib.rs\n-\t-\tassets/logo.png\n3\t0\tREADME.md\n");

    assert_eq!(
        stats,
        DiffStats {
            added: 15,
            removed: 4,
        }
    );
}

#[test]
fn worktree_branch_reads_live_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(args)
            .status();
        match status {
            Ok(status) => status.success(),
            Err(_) => false,
        }
    };
    if !git(&["init", "-q"]) {
        // No git on PATH (or init failed); the helper degrades to None,
        // which is the documented fallback. Nothing to assert.
        assert_eq!(worktree_branch(dir.path()), None);
        return;
    }
    let _ = git(&["config", "user.email", "t@example.com"]);
    let _ = git(&["config", "user.name", "t"]);
    let _ = git(&["checkout", "-q", "-b", "feature-migration"]);
    std::fs::write(dir.path().join("f"), "x").unwrap();
    let _ = git(&["add", "f"]);
    let _ = git(&["commit", "-q", "-m", "init"]);

    assert_eq!(
        worktree_branch(dir.path()).as_deref(),
        Some("feature-migration"),
        "the live branch is read from the worktree, overriding any pinned label"
    );
    // A non-repository path has no branch to track.
    let plain = tempfile::tempdir().unwrap();
    assert_eq!(worktree_branch(plain.path()), None);
}

#[test]
fn worktree_diff_stats_total_committed_staged_and_unstaged_over_trunk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap();
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(args)
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    };
    // `-b main` needs Git >= 2.28; an older git fails init and the helper
    // degrades to None, which is the documented fallback.
    if !git(&["init", "-q", "-b", "main"]) {
        assert_eq!(refresh_entry(path, 0, None).stats(), None);
        return;
    }
    let _ = git(&["config", "user.email", "t@example.com"]);
    let _ = git(&["config", "user.name", "t"]);
    let write = |name: &str, body: &str| std::fs::write(dir.path().join(name), body).unwrap();

    // Fork point on `main`: a three-line tracked file.
    write("base.txt", "a\nb\nc\n");
    let _ = git(&["add", "base.txt"]);
    let _ = git(&["commit", "-q", "-m", "base"]);
    let _ = git(&["branch", "feature-migration"]);

    // `main` advances *after* the fork — a merge-base diff must ignore this,
    // so it never shows up as the worktree's own churn.
    write("base.txt", "a\nB\nc\n");
    let _ = git(&["commit", "-aqm", "trunk moves on"]);

    let _ = git(&["checkout", "-q", "feature-migration"]);
    // Committed on the branch: a new two-line file.
    write("feat.txt", "x\ny\n");
    let _ = git(&["add", "feat.txt"]);
    let _ = git(&["commit", "-q", "-m", "feature work"]);
    // Staged but uncommitted: a new one-line file.
    write("staged.txt", "s\n");
    let _ = git(&["add", "staged.txt"]);
    // Unstaged: one more line appended to a tracked file.
    write("base.txt", "a\nb\nc\nd\n");

    let entry = refresh_entry(path, 0, None);
    assert_eq!(
        entry.stats(),
        Some(DiffStats {
            // +2 committed, +1 staged, +1 unstaged — all measured from the
            // fork point, none from main's post-fork commit.
            added: 4,
            removed: 0,
        }),
        "the header counts committed + staged + unstaged over the trunk merge-base"
    );
    // One commit on the branch since the fork point — staged/unstaged change
    // does not bump the commit count.
    assert_eq!(
        entry.commits,
        Some(1),
        "the commit count is the branch's commits ahead of the trunk merge-base"
    );
    // Main's one post-fork commit is the branch's behind count, and the
    // resolved trunk names the header's `≡` marker.
    assert_eq!(
        entry.behind,
        Some(1),
        "the behind count is the trunk's commits past the merge-base"
    );
    assert_eq!(entry.trunk.as_deref(), Some("main"));

    // A non-repository path has nothing to diff or count.
    let plain = tempfile::tempdir().unwrap();
    let plain_entry = refresh_entry(plain.path().to_str().unwrap(), 0, None);
    assert_eq!(plain_entry.stats(), None);
    assert_eq!(plain_entry.commits, None);
    assert_eq!(plain_entry.behind, None);
    assert_eq!(plain_entry.trunk, None);
}

#[test]
fn trunk_ladder_prefers_a_configured_branch_that_resolves() {
    let dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(args)
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    };
    if !git(&["init", "-q", "-b", "main"]) {
        assert_eq!(trunk_ref(dir.path(), Some("develop")), None);
        return;
    }
    let _ = git(&["config", "user.email", "t@example.com"]);
    let _ = git(&["config", "user.name", "t"]);
    std::fs::write(dir.path().join("f"), "x").unwrap();
    let _ = git(&["add", "f"]);
    let _ = git(&["commit", "-q", "-m", "init"]);
    let _ = git(&["branch", "develop"]);

    // The configured branch exists here, so it wins over `main`.
    assert_eq!(
        trunk_ref(dir.path(), Some("develop")).as_deref(),
        Some("develop")
    );
    // A machine-wide preference this repo lacks falls through to detection
    // rather than losing the repo's stats.
    assert_eq!(
        trunk_ref(dir.path(), Some("absent")).as_deref(),
        Some("main")
    );
    // An option-shaped name is never handed to git; detection stands alone.
    assert_eq!(
        trunk_ref(dir.path(), Some("--help")).as_deref(),
        Some("main")
    );
    assert_eq!(trunk_ref(dir.path(), None).as_deref(), Some("main"));
}

#[test]
fn list_worktree_roots_includes_a_checkout_outside_the_project() {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main");
    std::fs::create_dir_all(&main).unwrap();
    let git = |cwd: &Path, args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    };
    if !git(&main, &["init", "-q"]) {
        // No git on PATH; the helper degrades to an empty list, which leaves
        // the reducer's project_root prefix test to stand alone.
        assert!(list_worktree_roots(&main).is_empty());
        return;
    }
    let _ = git(&main, &["config", "user.email", "t@example.com"]);
    let _ = git(&main, &["config", "user.name", "t"]);
    std::fs::write(main.join("f"), "x").unwrap();
    let _ = git(&main, &["add", "f"]);
    let _ = git(&main, &["commit", "-q", "-m", "init"]);

    // A worktree parked OUTSIDE the project root (a sibling of `main`).
    let external = tmp.path().join("external-wt");
    let _ = git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            external.to_str().unwrap(),
            "-b",
            "feature",
        ],
    );

    let roots = list_worktree_roots(&main);
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let roots: Vec<PathBuf> = roots.iter().map(|r| canon(r)).collect();
    assert!(
        roots.contains(&canon(&main)),
        "the main checkout is one of the worktree roots"
    );
    assert!(
        roots.contains(&canon(&external)),
        "a worktree outside the project root is enumerated, so it groups as project-related"
    );

    // A non-repository path has no worktrees to list.
    let plain = tempfile::tempdir().unwrap();
    assert!(list_worktree_roots(plain.path()).is_empty());
}

fn write_snapshot_cache(path: &Path, session: &str, produced_at_ms: u64) {
    let cache = SnapshotCache {
        produced_at_ms,
        session_name: session.to_owned(),
        panes: Vec::new(),
    };
    atomic::write_temp_then_rename(path, &cache).expect("write snapshot cache");
}

#[test]
fn snapshot_cache_serves_a_fresh_same_session_entry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snapshot.json");
    write_snapshot_cache(&path, "rimz-query-engine", unix_now_ms());
    assert!(fresh_snapshot_cache(&path, "rimz-query-engine", None).is_some());
}

#[test]
fn snapshot_cache_misses_a_different_session() {
    // One session's panes must never be served to a sidebar pinned to
    // another — the Zellij backend stamps PaneRef.session_name from the
    // requested session, so a cross-session hit would mislabel panes.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snapshot.json");
    write_snapshot_cache(&path, "rimz-query-engine", unix_now_ms());
    assert!(fresh_snapshot_cache(&path, "rimz-other", None).is_none());
}

#[test]
fn snapshot_cache_misses_a_stale_entry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snapshot.json");
    let stale = unix_now_ms().saturating_sub(SNAPSHOT_CACHE_TTL.as_millis() as u64 + 1);
    write_snapshot_cache(&path, "rimz-query-engine", stale);
    assert!(fresh_snapshot_cache(&path, "rimz-query-engine", None).is_none());
}

#[test]
fn snapshot_cache_misses_before_requested_pane_freshness_floor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snapshot.json");
    let produced_at_ms = unix_now_ms();
    write_snapshot_cache(&path, "rimz-query-engine", produced_at_ms);

    assert!(
        fresh_snapshot_cache(&path, "rimz-query-engine", Some(produced_at_ms)).is_some(),
        "a cache produced at the requested floor is usable"
    );
    assert!(
        fresh_snapshot_cache(
            &path,
            "rimz-query-engine",
            Some(produced_at_ms.saturating_add(1)),
        )
        .is_none(),
        "a pane-sensitive wakeup rejects the pre-signal pane cache"
    );
}

#[test]
fn snapshot_cache_misses_when_absent_or_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snapshot.json");
    assert!(fresh_snapshot_cache(&path, "rimz-query-engine", None).is_none());
    std::fs::write(&path, b"{ not json").unwrap();
    assert!(fresh_snapshot_cache(&path, "rimz-query-engine", None).is_none());
}

#[test]
fn read_only_consumer_serves_a_stale_same_session_base() {
    // A `--no-produce` renderer holds the producer's last published base even
    // past the freshness TTL — it renders the last good frame rather than
    // forking its own `list-panes`. The fresh-only read (the producer's fast
    // path) misses the stale entry; the TTL-agnostic read still serves it.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snapshot.json");
    let stale = unix_now_ms().saturating_sub(SNAPSHOT_CACHE_TTL.as_millis() as u64 + 1);
    write_snapshot_cache(&path, "rimz-query-engine", stale);
    assert!(
        fresh_snapshot_cache(&path, "rimz-query-engine", None).is_none(),
        "the producer's fresh-only fast path skips a stale entry"
    );
    assert!(
        read_snapshot_cache(&path, "rimz-query-engine").is_some(),
        "the consumer's read serves the stale entry as last-good"
    );
}

/// A cache entry binding `pane_pid` under `command` with `start_ticks`, as the
/// prior tick records it.
fn binding_entry(pane_pid: u32, command: &str, start_ticks: u64) -> MetricsSampleEntry {
    MetricsSampleEntry {
        stats_pid: pane_pid,
        cpu_ticks: 0,
        io_bytes: 0,
        sampled_at_ms: 0,
        pane_pid: Some(pane_pid),
        pane_command: Some(command.to_owned()),
        root_start_ticks: Some(start_ticks),
        cpu_pct: None,
        io_bps: None,
        rss_kb: None,
    }
}

#[test]
fn cached_root_pid_restores_only_a_live_unchanged_binding() {
    let entry = binding_entry(42, "zsh", 777);
    let alive = |pid: u32| (pid == 42).then_some(777);
    // Hit: same foreground command, pid alive with the recorded starttime.
    assert_eq!(cached_root_pid(&entry, "zsh", &alive), Some(42));
    // The foreground changed: possible re-tenancy, re-derive through the walk.
    assert_eq!(cached_root_pid(&entry, "cargo build", &alive), None);
    // Pid gone.
    assert_eq!(cached_root_pid(&entry, "zsh", &|_| None), None);
    // Pid recycled: alive, but a different starttime — never a stranger's pid.
    assert_eq!(cached_root_pid(&entry, "zsh", &|_| Some(778)), None);
    // An old cache shape with no binding recorded re-derives.
    let unbound = MetricsSampleEntry {
        pane_pid: None,
        root_start_ticks: None,
        ..binding_entry(42, "zsh", 777)
    };
    assert_eq!(cached_root_pid(&unbound, "zsh", &alive), None);
}

#[test]
fn stable_panes_restore_their_bindings_and_skip_the_walk() {
    // The steady-state contract: every pidless pane hits its guarded binding,
    // so the tick walks zero processes.
    let mut panes = vec![
        pane("terminal_1", Some("zsh"), Some("/repo")),
        pane("terminal_2", Some("node claude"), Some("/repo")),
    ];
    let mut prior = MetricsSampleCache::default();
    prior
        .entries
        .insert(panes[0].pane_id.to_string(), binding_entry(42, "zsh", 700));
    prior.entries.insert(
        panes[1].pane_id.to_string(),
        binding_entry(43, "node claude", 701),
    );
    let starts = |pid: u32| match pid {
        42 => Some(700),
        43 => Some(701),
        _ => None,
    };

    let needs_walk = restore_cached_bindings(&mut panes, &prior, &starts);

    assert!(!needs_walk, "an all-hit room never walks the process table");
    assert_eq!(panes[0].pane_pid, Some(42));
    assert_eq!(panes[1].pane_pid, Some(43));
}

#[test]
fn binding_misses_drive_the_walk_and_unbindable_panes_do_not() {
    // A pidless pane with no usable binding needs the walk…
    let mut missing = vec![pane("terminal_2", Some("zsh"), None)];
    assert!(restore_cached_bindings(
        &mut missing,
        &MetricsSampleCache::default(),
        &|_| None,
    ));
    assert_eq!(missing[0].pane_pid, None, "a miss restores nothing");

    // …while panes the walk could never bind — no command, sidebar chrome —
    // never trigger it, and a natively-pidded (tmux) pane is left alone.
    let mut pidded = pane("terminal_9", Some("zsh"), None);
    pidded.pane_pid = Some(9);
    let mut inert = vec![
        pane("terminal_3", None, None),
        pane(
            "terminal_4",
            Some(rimz::mux::zellij::SIDEBAR_PANE_NAME),
            None,
        ),
        pidded,
    ];
    assert!(!restore_cached_bindings(
        &mut inert,
        &MetricsSampleCache::default(),
        &|_| None,
    ));
    assert_eq!(inert[2].pane_pid, Some(9));
}

// ── Metrics sampling cadence (METRICS_SAMPLE_TTL) ───────────────────────────────

#[test]
fn metrics_cache_expires_after_sample_ttl() {
    let cache = MetricsSampleCache {
        sampled_at_ms: 1_000,
        entries: HashMap::new(),
    };
    let ttl_ms = METRICS_SAMPLE_TTL.as_millis() as u64;
    // Boundary-exact: fresh at exactly the TTL, due one ms past it.
    assert!(cache.is_fresh(1_000 + ttl_ms));
    assert!(!cache.is_fresh(1_001 + ttl_ms));
    // A clock that ran backwards reads fresh (saturating), never a re-sample
    // every tick.
    assert!(cache.is_fresh(500));
    // A pre-stamp cache (serde-default 0) is due on any real wall clock.
    assert!(!MetricsSampleCache::default().is_fresh(unix_now_ms()));
}

/// The within-TTL skip path: stored display values — and the root-pid binding
/// the process-row name anchors on — carry forward onto the matching pane, the
/// re-tenancy guard blanks a reused pane id, an uncached pane keeps its
/// `None`s, a natively-pidded pane keeps its own pid, and the cache file is
/// left unwritten.
#[test]
fn metrics_within_ttl_carries_display_values_forward() {
    let dir = tempfile::TempDir::new().unwrap();
    let runtime = rimz::RuntimePaths::under(
        rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/metrics-ttl")),
        dir.path(),
    )
    .unwrap();
    std::fs::create_dir_all(&runtime.root).unwrap();

    let mut panes = vec![
        pane("terminal_1", Some("zsh"), Some("/repo")),
        pane("terminal_2", Some("cargo build"), Some("/repo")),
        pane("terminal_3", Some("zsh"), Some("/repo")),
        pane("terminal_4", Some("zsh"), Some("/repo")),
    ];
    // terminal_4 reports its pid natively (the tmux case): the carry must
    // never overwrite a live read with a cached binding.
    panes[3].pane_pid = Some(7);
    let mut cache = MetricsSampleCache {
        sampled_at_ms: unix_now_ms(),
        entries: HashMap::new(),
    };
    cache.entries.insert(
        panes[0].pane_id.to_string(),
        MetricsSampleEntry {
            cpu_pct: Some(42),
            io_bps: Some(1_024),
            rss_kb: Some(2_048),
            ..binding_entry(42, "zsh", 700)
        },
    );
    // terminal_2's entry recorded a different foreground: the pane id was
    // re-tenanted inside the window, so its stats must not carry.
    cache.entries.insert(
        panes[1].pane_id.to_string(),
        MetricsSampleEntry {
            cpu_pct: Some(99),
            io_bps: Some(9_999),
            rss_kb: Some(9_999),
            ..binding_entry(43, "zsh", 701)
        },
    );
    cache
        .entries
        .insert(panes[3].pane_id.to_string(), binding_entry(44, "zsh", 702));
    let cache_path = runtime.root.join("metrics-sample.json");
    std::fs::write(&cache_path, serde_json::to_vec(&cache).unwrap()).unwrap();
    let written = std::fs::read(&cache_path).unwrap();

    enrich_pane_metrics(&mut panes, "rimz-query-engine", &runtime);

    assert_eq!(panes[0].cpu_pct, Some(42), "matching pane carries forward");
    assert_eq!(panes[0].io_bps, Some(1_024));
    assert_eq!(panes[0].rss_kb, Some(2_048));
    assert_eq!(
        panes[0].pane_pid,
        Some(42),
        "the root-pid binding rides with the values — the process-row name \
         anchor must not flip between windows"
    );
    assert_eq!(
        panes[1].cpu_pct, None,
        "re-tenanted pane id carries nothing"
    );
    assert_eq!(
        panes[1].pane_pid, None,
        "a re-tenanted pane id carries no binding either"
    );
    assert_eq!(panes[2].cpu_pct, None, "uncached pane warms up next window");
    assert_eq!(
        panes[3].pane_pid,
        Some(7),
        "a natively-reported pid is never overwritten by the cached binding"
    );
    assert_eq!(
        std::fs::read(&cache_path).unwrap(),
        written,
        "the skip path never rewrites the sample cache"
    );
}

/// A due cache (stamp older than the TTL) re-samples and re-stamps, so the
/// next produce inside the new window skips again.
#[test]
fn metrics_due_path_resamples_and_restamps() {
    let dir = tempfile::TempDir::new().unwrap();
    let runtime = rimz::RuntimePaths::under(
        rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/metrics-due")),
        dir.path(),
    )
    .unwrap();
    std::fs::create_dir_all(&runtime.root).unwrap();

    let stale_ms = unix_now_ms() - METRICS_SAMPLE_TTL.as_millis() as u64 - 1_000;
    let cache = MetricsSampleCache {
        sampled_at_ms: stale_ms,
        entries: HashMap::new(),
    };
    let cache_path = runtime.root.join("metrics-sample.json");
    std::fs::write(&cache_path, serde_json::to_vec(&cache).unwrap()).unwrap();

    let mut panes = vec![pane("terminal_1", Some("zsh"), Some("/repo"))];
    enrich_pane_metrics(&mut panes, "rimz-query-engine", &runtime);

    let rewritten: MetricsSampleCache =
        serde_json::from_slice(&std::fs::read(&cache_path).unwrap()).unwrap();
    assert!(
        rewritten.sampled_at_ms > stale_ms,
        "a due produce re-samples and re-stamps the cache"
    );
}
