use super::*;
use crate::sidebar::produce::test_support::pane;

/// A process-table entry for the pid-backfill matcher fixtures; everything
/// runs as one uid (1000) unless a test says otherwise.
fn proc_info(pid: u32, ppid: u32, cmdline: &str) -> crate::proc::ProcInfo {
    crate::proc::ProcInfo {
        pid,
        ppid,
        real_uid: 1000,
        cmdline: cmdline.to_owned(),
    }
}

/// The ppid→children map `enrich_pane_metrics` builds, over a fixture table.
fn children_of(procs: &[crate::proc::ProcInfo]) -> HashMap<u32, Vec<u32>> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in procs {
        children.entry(p.ppid).or_default().push(p.pid);
    }
    children
}

/// The session's Zellij server process, socket named after the session.
fn server(pid: u32, session: &str) -> crate::proc::ProcInfo {
    proc_info(
        pid,
        1,
        &format!("/usr/bin/zellij --server /run/user/1000/zellij/contract_version_1/{session}"),
    )
}

const SESSION: &str = "rimz-query-engine";

fn frame_from_panes(panes: Vec<crate::feed::PaneRef>) -> crate::sidebar::frame::PaneFrame {
    crate::sidebar::frame::assemble_frame(panes, 1, SESSION)
}

fn pane_id(raw: &str) -> crate::ids::PaneId {
    crate::ids::PaneId::from_parts(crate::ids::MuxName::Zellij, raw)
}

fn state<'a>(
    frame: &'a crate::sidebar::frame::PaneFrame,
    raw: &str,
) -> &'a crate::sidebar::frame::PaneState {
    let pane_id = pane_id(raw);
    frame
        .pane_states()
        .find(|pane| pane.pane_id == pane_id)
        .expect("pane state exists")
}

fn sync_panes_from_frame(
    panes: &mut [crate::feed::PaneRef],
    frame: crate::sidebar::frame::PaneFrame,
) {
    let mut projected: HashMap<crate::ids::PaneId, crate::feed::PaneRef> = frame
        .to_pane_refs()
        .into_iter()
        .map(|pane| (pane.pane_id.clone(), pane))
        .collect();
    for slot in panes {
        if let Some(next) = projected.remove(&slot.pane_id) {
            *slot = next;
        }
    }
}

fn backfill(
    panes: &mut [crate::feed::PaneRef],
    procs: &[crate::proc::ProcInfo],
    cwds: &[(u32, &str)],
) {
    let cwds: HashMap<u32, PathBuf> = cwds
        .iter()
        .map(|(pid, cwd)| (*pid, PathBuf::from(cwd)))
        .collect();
    let mut frame = frame_from_panes(panes.to_vec());
    backfill_zellij_pane_pids(
        &mut frame,
        procs,
        &children_of(procs),
        SESSION,
        Some(1000),
        &|pid| cwds.get(&pid).cloned(),
    );
    sync_panes_from_frame(panes, frame);
}

#[test]
fn resolve_candidate_root_accepts_one_root_without_cwd() {
    let candidates = [(300, 200), (301, 200)];

    let matched = resolve_candidate_root(&candidates, None, &|_| panic!("cwd is not needed"));

    assert_eq!(matched, Some(200));
}

#[test]
fn resolve_candidate_root_abstains_without_candidates() {
    let matched = resolve_candidate_root(&[], Some("/repo"), &|_| {
        panic!("empty candidates do not need cwd")
    });

    assert_eq!(matched, None);
}

#[test]
fn resolve_candidate_root_uses_cwd_to_narrow_plural_roots() {
    let candidates = [(300, 200), (310, 210)];

    let matched = resolve_candidate_root(&candidates, Some("/repo/feature"), &|pid| match pid {
        300 => Some(PathBuf::from("/repo/main")),
        310 => Some(PathBuf::from("/repo/feature")),
        _ => None,
    });

    assert_eq!(matched, Some(210));
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

/// A cache entry binding `pane_pid` with `start_ticks`, as the prior tick
/// records it. `command` is the sample-time foreground the carry guard keys on.
fn binding_entry(pane_pid: u32, start_ticks: u64, command: &str) -> MetricsSampleEntry {
    MetricsSampleEntry {
        stats_pid: pane_pid,
        cpu_ticks: 0,
        io_bytes: 0,
        sampled_at_ms: 0,
        pane_pid: Some(pane_pid),
        root_start_ticks: Some(start_ticks),
        command: Some(command.to_owned()),
        cpu_pct: None,
        io_bps: None,
        rss_kb: None,
        state_char: None,
        process_state: None,
    }
}

fn fresh_entry(
    pane_pid: u32,
    start_ticks: u64,
    command: &str,
    sampled_at_ms: u64,
) -> MetricsSampleEntry {
    MetricsSampleEntry {
        sampled_at_ms,
        ..binding_entry(pane_pid, start_ticks, command)
    }
}

#[test]
fn process_state_marks_zombie_and_persistent_uninterruptible_sleep_as_stuck() {
    assert_eq!(
        process_state_from_stat(Some('Z'), None),
        Some(ProcessState::Stuck)
    );
    assert_eq!(process_state_from_stat(Some('D'), None), None);
    assert_eq!(
        process_state_from_stat(Some('D'), Some('D')),
        Some(ProcessState::Stuck)
    );
    assert_eq!(process_state_from_stat(Some('R'), Some('D')), None);
    assert_eq!(process_state_from_stat(None, Some('D')), None);
}

#[test]
fn cached_root_pid_restores_only_a_live_unchanged_binding() {
    let entry = binding_entry(42, 777, "zsh");
    let alive = |pid: u32| (pid == 42).then_some(777);
    // Hit: pid alive with the recorded starttime.
    assert_eq!(cached_root_pid(&entry, &alive), Some(42));
    // Pid gone.
    assert_eq!(cached_root_pid(&entry, &|_| None), None);
    // Pid recycled: alive, but a different starttime — never a stranger's pid.
    assert_eq!(cached_root_pid(&entry, &|_| Some(778)), None);
    // An old cache shape with no binding recorded re-derives.
    let unbound = MetricsSampleEntry {
        pane_pid: None,
        root_start_ticks: None,
        ..binding_entry(42, 777, "zsh")
    };
    assert_eq!(cached_root_pid(&unbound, &alive), None);
}

/// The due set `enrich_pane_metrics` builds when every sampleable pane is due
/// — the cold-cache shape the restore tests exercise.
fn all_sampleable_due(frame: &crate::sidebar::frame::PaneFrame) -> HashSet<String> {
    frame
        .pane_states()
        .filter(|pane| pane_sampleable(pane))
        .map(|pane| pane.pane_id.to_string())
        .collect()
}

#[test]
fn stable_panes_restore_their_bindings_and_skip_the_walk() {
    // The steady-state contract: every due pidless pane hits its guarded
    // binding, so the tick walks zero processes.
    let panes = vec![
        pane("terminal_1", Some("zsh"), Some("/repo")),
        pane("terminal_2", Some("node claude"), Some("/repo")),
    ];
    let mut frame = frame_from_panes(panes.clone());
    let mut prior = MetricsSampleCache::default();
    prior
        .entries
        .insert(panes[0].pane_id.to_string(), binding_entry(42, 700, "zsh"));
    prior.entries.insert(
        panes[1].pane_id.to_string(),
        binding_entry(43, 701, "node claude"),
    );
    let starts = |pid: u32| match pid {
        42 => Some(700),
        43 => Some(701),
        _ => None,
    };

    let due = all_sampleable_due(&frame);
    let needs_walk = restore_cached_bindings(&mut frame, &prior, &due, &starts);

    assert!(!needs_walk, "an all-hit room never walks the process table");
    assert_eq!(state(&frame, "terminal_1").current.pid, Some(42));
    assert_eq!(state(&frame, "terminal_2").current.pid, Some(43));
}

#[test]
fn binding_misses_drive_the_walk_and_unbindable_panes_do_not() {
    // A due pidless pane with no usable binding needs the walk…
    let mut missing = frame_from_panes(vec![pane("terminal_2", Some("zsh"), None)]);
    let due = all_sampleable_due(&missing);
    assert!(restore_cached_bindings(
        &mut missing,
        &MetricsSampleCache::default(),
        &due,
        &|_| None,
    ));
    assert_eq!(
        state(&missing, "terminal_2").current.pid,
        None,
        "a miss restores nothing"
    );

    // …while panes the walk could never bind — no command, sidebar chrome —
    // sit outside the sampleable due set, and a natively-pidded (tmux) pane
    // is left alone.
    let mut pidded = pane("terminal_9", Some("zsh"), None);
    pidded.pane_pid = Some(9);
    let mut inert = frame_from_panes(vec![
        pane("terminal_3", None, None),
        pane(
            "terminal_4",
            Some(crate::mux::zellij::SIDEBAR_PANE_NAME),
            None,
        ),
        pidded,
    ]);
    let due = all_sampleable_due(&inert);
    assert!(!restore_cached_bindings(
        &mut inert,
        &MetricsSampleCache::default(),
        &due,
        &|_| None,
    ));
    assert_eq!(state(&inert, "terminal_9").current.pid, Some(9));
}

#[test]
fn unbound_panes_between_samples_never_drive_the_walk() {
    // The mixed-room case: a hot pane is due while an idle unbound pane's
    // fresh entry is not — the missing binding must not drag the table walk
    // back onto every hot tick.
    let panes = vec![
        pane("terminal_1", Some("cargo build"), Some("/repo")),
        pane("terminal_2", Some("zsh"), Some("/repo")),
    ];
    let mut frame = frame_from_panes(panes.clone());
    let mut prior = MetricsSampleCache::default();
    prior.entries.insert(
        panes[0].pane_id.to_string(),
        binding_entry(42, 700, "cargo build"),
    );
    prior.entries.insert(
        panes[1].pane_id.to_string(),
        unbound_entry(Some("zsh".to_owned()), 1_000),
    );
    let due = HashSet::from([panes[0].pane_id.to_string()]);

    let needs_walk =
        restore_cached_bindings(&mut frame, &prior, &due, &|pid| (pid == 42).then_some(700));

    assert!(
        !needs_walk,
        "the due pane restored its binding; the unbound idle pane backs off"
    );
    assert_eq!(state(&frame, "terminal_1").current.pid, Some(42));
    assert_eq!(state(&frame, "terminal_2").current.pid, None);
}

// ── Metrics sampling cadence ───────────────────────────────────────────────────

#[test]
fn metric_entry_due_samples_missing_and_changed_entries_immediately() {
    let command = Some("zsh".to_owned());
    let entry = fresh_entry(42, 700, "zsh", 1_000);

    assert!(metric_entry_due(None, &command, 1_000));
    assert!(metric_entry_due(
        Some(&entry),
        &Some("cargo build".to_owned()),
        1_001
    ));
    assert!(
        !metric_entry_due(Some(&entry), &command, 500),
        "a clock that ran backwards reads fresh rather than busy-looping"
    );
}

#[test]
fn idle_entries_use_the_idle_metrics_ttl() {
    let command = Some("zsh".to_owned());
    let entry = fresh_entry(42, 700, "zsh", 1_000);
    let ttl_ms = METRICS_SAMPLE_TTL.as_millis() as u64;

    assert!(!metric_entry_due(Some(&entry), &command, 1_000 + ttl_ms));
    assert!(metric_entry_due(Some(&entry), &command, 1_001 + ttl_ms));
}

#[test]
fn active_and_child_entries_use_the_hot_metrics_ttl() {
    let active = Some("cargo build".to_owned());
    let active_entry = fresh_entry(42, 700, "cargo build", 1_000);
    let ttl_ms = METRICS_HOT_SAMPLE_TTL.as_millis() as u64;

    assert!(!metric_entry_due(
        Some(&active_entry),
        &active,
        1_000 + ttl_ms
    ));
    assert!(metric_entry_due(
        Some(&active_entry),
        &active,
        1_001 + ttl_ms
    ));

    let child_command = Some("htop".to_owned());
    let mut child_entry = fresh_entry(42, 700, "htop", 1_000);
    child_entry.stats_pid = 99;
    assert!(metrics_entry_hot(&child_entry));
    assert!(metric_entry_due(
        Some(&child_entry),
        &child_command,
        1_001 + ttl_ms
    ));
}

#[test]
fn cached_entry_publishes_resource_stats_only_when_complete() {
    let mut frame = frame_from_panes(vec![pane("terminal_1", Some("zsh"), Some("/repo"))]);
    let mut partial = fresh_entry(42, 700, "zsh", 1_000);
    partial.rss_kb = Some(2_048);
    partial.cpu_pct = Some(3);

    apply_cached_entry(frame.pane_states_mut().next().unwrap(), &partial);
    assert_eq!(state(&frame, "terminal_1").metrics.rss_kb, None);
    assert_eq!(state(&frame, "terminal_1").metrics.cpu_pct, None);
    assert_eq!(state(&frame, "terminal_1").metrics.io_bps, None);

    let mut complete = partial;
    complete.io_bps = Some(512);
    apply_cached_entry(frame.pane_states_mut().next().unwrap(), &complete);
    assert_eq!(state(&frame, "terminal_1").metrics.rss_kb, Some(2_048));
    assert_eq!(state(&frame, "terminal_1").metrics.cpu_pct, Some(3));
    assert_eq!(state(&frame, "terminal_1").metrics.io_bps, Some(512));
}

#[test]
fn unbound_idle_panes_back_off_on_the_idle_ttl() {
    let dir = tempfile::TempDir::new().unwrap();
    let runtime = crate::RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/metrics-unbound")),
        dir.path(),
    )
    .unwrap();
    std::fs::create_dir_all(&runtime.root).unwrap();

    let mut frame = frame_from_panes(vec![pane("terminal_1", Some("zsh"), Some("/repo"))]);
    assert!(enrich_pane_metrics(
        &mut frame,
        "rimz-test-no-such-session-for-metrics",
        &runtime
    ));
    assert!(
        !pane_metrics_due(&frame, &runtime),
        "an unbound idle pane records a sample attempt instead of retrying the \
         table walk every produce"
    );

    let cache_path = runtime.root.join("metrics-sample.json");
    let cache: MetricsSampleCache =
        serde_json::from_slice(&std::fs::read(&cache_path).unwrap()).unwrap();
    let entry = cache
        .entries
        .get(&state(&frame, "terminal_1").pane_id.to_string())
        .unwrap();
    assert_eq!(entry.stats_pid, 0);
    assert_eq!(entry.pane_pid, None);
}

/// The within-TTL skip path: stored display values — and the root-pid binding
/// the process-row name anchors on — carry forward onto the matching pane, the
/// sample-time command guard sends a changed foreground straight to warmup
/// (even on the same root pid — the tmux shell survives every foreground
/// change), an uncached pane keeps its `None`s, and a natively-pidded pane
/// keeps its own pid.
#[test]
fn metrics_within_ttl_carries_display_values_forward() {
    let dir = tempfile::TempDir::new().unwrap();
    let runtime = crate::RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/metrics-ttl")),
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
    // terminal_2 is the tmux foreground change: the root pid (the shell) is
    // native and unchanged, only the sampled command differs.
    panes[1].pane_pid = Some(43);
    // terminal_4 reports its pid natively (the tmux case): the carry must
    // never overwrite a live read with a cached binding.
    panes[3].pane_pid = Some(7);
    let now_ms = unix_now_ms();
    let mut cache = MetricsSampleCache {
        sampled_at_ms: now_ms,
        entries: HashMap::new(),
    };
    cache.entries.insert(
        panes[0].pane_id.to_string(),
        MetricsSampleEntry {
            cpu_pct: Some(42),
            io_bps: Some(1_024),
            rss_kb: Some(2_048),
            ..fresh_entry(42, 700, "zsh", now_ms)
        },
    );
    // terminal_2's entry sampled the prior foreground (`zsh`, now `cargo
    // build` on the same shell root): the values belong to the old tenant and
    // must not carry — pid identity alone could never tell.
    cache.entries.insert(
        panes[1].pane_id.to_string(),
        MetricsSampleEntry {
            cpu_pct: Some(99),
            io_bps: Some(9_999),
            rss_kb: Some(9_999),
            ..fresh_entry(43, 701, "zsh", now_ms)
        },
    );
    cache.entries.insert(
        panes[3].pane_id.to_string(),
        fresh_entry(44, 702, "zsh", now_ms),
    );
    let cache_path = runtime.root.join("metrics-sample.json");
    std::fs::write(&cache_path, serde_json::to_vec(&cache).unwrap()).unwrap();
    let mut frame = frame_from_panes(panes);

    assert!(enrich_pane_metrics(
        &mut frame,
        "rimz-query-engine",
        &runtime
    ));

    assert_eq!(
        state(&frame, "terminal_1").metrics.cpu_pct,
        Some(42),
        "matching pane carries forward"
    );
    assert_eq!(state(&frame, "terminal_1").metrics.io_bps, Some(1_024));
    assert_eq!(state(&frame, "terminal_1").metrics.rss_kb, Some(2_048));
    assert_eq!(
        state(&frame, "terminal_1").current.pid,
        Some(42),
        "the root-pid binding rides with the values — the process-row name \
         anchor must not flip between windows"
    );
    assert_eq!(
        state(&frame, "terminal_2").metrics.cpu_pct,
        None,
        "a changed foreground on the same root pid publishes no partial stats"
    );
    assert_eq!(
        state(&frame, "terminal_3").metrics.cpu_pct,
        None,
        "uncached pane warms up next window"
    );
    assert_eq!(
        state(&frame, "terminal_4").current.pid,
        Some(7),
        "a natively-reported pid is never overwritten by the cached binding"
    );
    let rewritten: MetricsSampleCache =
        serde_json::from_slice(&std::fs::read(&cache_path).unwrap()).unwrap();
    assert_eq!(
        rewritten
            .entries
            .get(&state(&frame, "terminal_1").pane_id.to_string())
            .unwrap()
            .sampled_at_ms,
        now_ms,
        "carried entries keep their original sample time"
    );
    assert_eq!(
        rewritten
            .entries
            .get(&state(&frame, "terminal_2").pane_id.to_string())
            .and_then(|entry| entry.command.as_deref()),
        Some("cargo build"),
        "changed commands get a fresh raw-counter entry immediately"
    );
}

/// A due cache (stamp older than the TTL) re-samples and re-stamps, so the
/// next produce inside the new window skips again.
#[test]
fn metrics_due_path_resamples_and_restamps() {
    let dir = tempfile::TempDir::new().unwrap();
    let runtime = crate::RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/metrics-due")),
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

    let mut pidded = pane("terminal_1", Some("zsh"), Some("/repo"));
    pidded.pane_pid = Some(std::process::id());
    let mut frame = frame_from_panes(vec![pidded]);
    assert!(enrich_pane_metrics(
        &mut frame,
        "rimz-query-engine",
        &runtime
    ));

    let rewritten: MetricsSampleCache =
        serde_json::from_slice(&std::fs::read(&cache_path).unwrap()).unwrap();
    assert!(
        rewritten.sampled_at_ms > stale_ms,
        "a due produce re-samples and re-stamps the cache"
    );
}
