use super::*;
use crate::sidebar::refresh::git_stats::{DiffStatsCache, DiffStatsCacheEntry};

#[test]
fn failure_ttl_escalates_to_success_ttl_cap() {
    assert_eq!(pr_state_failure_ttl(0, PR_STATE_TTL), PR_STATE_RETRY_TTL);
    assert_eq!(pr_state_failure_ttl(1, PR_STATE_TTL), PR_STATE_RETRY_TTL);
    assert_eq!(
        pr_state_failure_ttl(2, PR_STATE_TTL),
        Duration::from_secs(60)
    );
    assert_eq!(
        pr_state_failure_ttl(3, PR_STATE_TTL),
        Duration::from_secs(120)
    );
    assert_eq!(
        pr_state_failure_ttl(4, PR_STATE_TTL),
        Duration::from_secs(240)
    );
    assert_eq!(pr_state_failure_ttl(5, PR_STATE_TTL), PR_STATE_TTL);
    assert_eq!(pr_state_failure_ttl(u32::MAX, PR_STATE_TTL), PR_STATE_TTL);
    assert_eq!(pr_state_failure_ttl(1, PR_STATE_HOT_TTL), PR_STATE_HOT_TTL);
}

#[test]
fn failure_counter_resets_on_success_and_saturates_on_failure() {
    let mut prior = RepoProbe {
        ok: false,
        consecutive_failures: 7,
        ..RepoProbe::default()
    };

    assert_eq!(next_consecutive_failures(Some(&prior), true), 0);
    assert_eq!(next_consecutive_failures(Some(&prior), false), 8);
    assert_eq!(next_consecutive_failures(None, false), 1);

    prior.consecutive_failures = u32::MAX;
    assert_eq!(next_consecutive_failures(Some(&prior), false), u32::MAX);
}

#[test]
fn repo_due_tracks_fresh_stale_nudge_uncached_and_failure_backoff() {
    let ttl = Duration::from_secs(20);
    let ttl_ms = ttl.as_millis() as u64;
    let probe = RepoProbe {
        refreshed_at_ms: 1_000,
        ok: true,
        consecutive_failures: 0,
    };

    assert!(!repo_due(Some(&probe), ttl, 1_000 + ttl_ms, false, false));
    assert!(repo_due(Some(&probe), ttl, 1_001 + ttl_ms, false, false));
    assert!(repo_due(None, ttl, 1_000, false, false));
    assert!(repo_due(Some(&probe), ttl, 1_000, true, false));
    assert!(repo_due(Some(&probe), ttl, 1_000, false, true));

    let failed = RepoProbe {
        refreshed_at_ms: 1_000,
        ok: false,
        consecutive_failures: 1,
    };
    assert!(!repo_due(Some(&failed), ttl, 1_000 + ttl_ms, false, false));
    assert!(repo_due(Some(&failed), ttl, 1_001 + ttl_ms, false, false));
}

#[test]
fn assign_states_handles_open_terminal_transition_closed_and_absent() {
    let targets = vec![
        target("/repo/open", "open"),
        target("/repo/merged-terminal", "merged-terminal"),
        target("/repo/merged-no-ci", "merged-no-ci"),
        target("/repo/merged-pending", "merged-pending"),
        target("/repo/merged-legacy", "merged-legacy"),
        target("/repo/transition", "transition"),
        target("/repo/closed", "closed"),
        target("/repo/none", "none"),
    ];
    let mut open_map = BTreeMap::new();
    open_map.insert(
        "open".to_owned(),
        forge::PrCandidate {
            number: 91,
            state: WorktreePrState::Open,
            ci: Some(WorktreePrCi::Passing),
            merge_sha: None,
        },
    );
    let mut prior = BTreeMap::new();
    prior.insert(
        "/repo/merged-terminal".to_owned(),
        PrLink {
            state: WorktreePrState::Merged,
            number: Some(80),
            ci: Some(WorktreePrCi::Failing),
            merge_sha: Some("terminal-sha".to_owned()),
        },
    );
    prior.insert(
        "/repo/merged-no-ci".to_owned(),
        PrLink {
            state: WorktreePrState::Merged,
            number: Some(77),
            ci: None,
            merge_sha: Some("no-ci-sha".to_owned()),
        },
    );
    prior.insert(
        "/repo/merged-pending".to_owned(),
        PrLink {
            state: WorktreePrState::Merged,
            number: Some(79),
            ci: Some(WorktreePrCi::Pending),
            merge_sha: Some("pending-sha".to_owned()),
        },
    );
    prior.insert(
        "/repo/merged-legacy".to_owned(),
        PrLink {
            state: WorktreePrState::Merged,
            number: Some(78),
            ci: None,
            merge_sha: None,
        },
    );
    prior.insert(
        "/repo/transition".to_owned(),
        PrLink {
            state: WorktreePrState::Open,
            number: Some(81),
            ci: Some(WorktreePrCi::Pending),
            merge_sha: None,
        },
    );
    prior.insert(
        "/repo/closed".to_owned(),
        PrLink {
            state: WorktreePrState::Closed,
            number: Some(82),
            ci: None,
            merge_sha: None,
        },
    );

    let assigned = assign_states(&targets, &open_map, &prior);

    assert_eq!(
        assigned.states.get("/repo/open"),
        Some(&PrLink {
            state: WorktreePrState::Open,
            number: Some(91),
            ci: Some(WorktreePrCi::Passing),
            merge_sha: None,
        })
    );
    assert_eq!(
        assigned.states.get("/repo/merged-terminal"),
        Some(&PrLink {
            state: WorktreePrState::Merged,
            number: Some(80),
            ci: Some(WorktreePrCi::Failing),
            merge_sha: Some("terminal-sha".to_owned()),
        })
    );
    assert_eq!(
        assigned.states.get("/repo/merged-no-ci"),
        Some(&PrLink {
            state: WorktreePrState::Merged,
            number: Some(77),
            ci: None,
            merge_sha: Some("no-ci-sha".to_owned()),
        })
    );
    assert_eq!(
        assigned.states.get("/repo/closed"),
        Some(&PrLink {
            state: WorktreePrState::Closed,
            number: Some(82),
            ci: None,
            merge_sha: None,
        })
    );
    assert!(!assigned.states.contains_key("/repo/none"));
    assert_eq!(
        assigned
            .transitions
            .iter()
            .map(|target| target.path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "/repo/merged-pending",
            "/repo/merged-legacy",
            "/repo/transition"
        ]
    );
}

#[test]
fn legacy_cache_defaults_and_leaves_repos_due() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pr-state.json");
    std::fs::write(
        &path,
        r#"{
            "refreshed_at_ms": 1000,
            "ok": true,
            "consecutive_failures": 0,
            "states": {"/repo/a": "open"}
        }"#,
    )
    .unwrap();
    let cache = read_pr_state_cache(&path);
    let groups = group_targets(vec![target("/repo/a", "a")]);

    assert!(cache.states.is_empty());
    assert!(cache.repos.is_empty());
    assert!(cache.head_seen.is_empty());
    assert!(
        due_repo_keys(&groups, &cache, &BTreeSet::new(), &BTreeSet::new(), 1_000)
            .contains("gh:github.com:org/repo")
    );
}

#[test]
fn legacy_pr_link_without_ci_defaults_to_unknown() {
    let link: PrLink = serde_json::from_str(r#"{"state":"open","number":91}"#).unwrap();

    assert_eq!(link.ci, None);
    assert_eq!(link.merge_sha, None);
}

#[test]
fn pending_ci_keeps_repo_on_hot_ttl() {
    let repo_key = "gh:github.com:org/repo".to_owned();
    let path = "/repo/a".to_owned();
    let mut cache = PrStateCache::default();
    cache.repos.insert(
        repo_key.clone(),
        RepoProbe {
            refreshed_at_ms: 1_000,
            ok: true,
            consecutive_failures: 0,
        },
    );
    cache.path_repos.insert(path.clone(), repo_key.clone());
    cache.head_seen.insert(path.clone(), String::new());
    let needed = vec![path];
    let groups = group_targets(vec![target("/repo/a", "a")]);
    let now_ms = 1_001 + PR_STATE_HOT_TTL.as_millis() as u64;

    for state in [WorktreePrState::Open, WorktreePrState::Merged] {
        cache.states.insert(
            needed[0].clone(),
            PrLink {
                state,
                number: Some(91),
                ci: Some(WorktreePrCi::Pending),
                merge_sha: (state == WorktreePrState::Merged).then(|| "merged-sha".to_owned()),
            },
        );
        assert!(
            cached_due_repo_keys(
                &cache,
                &needed,
                &DiffStatsCache::default(),
                &BTreeSet::new(),
                &BTreeSet::new(),
                now_ms,
            )
            .unwrap()
            .contains(&repo_key)
        );
        assert!(
            due_repo_keys(&groups, &cache, &BTreeSet::new(), &BTreeSet::new(), now_ms,)
                .contains(&repo_key)
        );
    }
}

#[test]
fn cached_due_uses_head_nudge_without_git_metadata() {
    let mut cache = PrStateCache::default();
    cache.repos.insert(
        "gh:github.com:org/repo".to_owned(),
        RepoProbe {
            refreshed_at_ms: 1_000,
            ok: true,
            consecutive_failures: 0,
        },
    );
    cache
        .head_seen
        .insert("/repo/a".to_owned(), "old".to_owned());
    cache
        .path_repos
        .insert("/repo/a".to_owned(), "gh:github.com:org/repo".to_owned());
    let mut diff = DiffStatsCache::default();
    diff.entries.insert(
        "/repo/a".to_owned(),
        DiffStatsCacheEntry {
            head_sha: Some("old".to_owned()),
            ..DiffStatsCacheEntry::default()
        },
    );
    let needed = vec!["/repo/a".to_owned()];

    assert!(
        cached_due_repo_keys(
            &cache,
            &needed,
            &diff,
            &BTreeSet::new(),
            &BTreeSet::new(),
            1_000 + PR_STATE_TTL.as_millis() as u64
        )
        .unwrap()
        .is_empty()
    );

    diff.entries.get_mut("/repo/a").unwrap().head_sha = Some("new".to_owned());

    assert!(
        cached_due_repo_keys(
            &cache,
            &needed,
            &diff,
            &BTreeSet::new(),
            &BTreeSet::new(),
            1_000
        )
        .unwrap()
        .contains("gh:github.com:org/repo")
    );
}

#[test]
fn uncached_path_requires_target_assembly() {
    let cache = PrStateCache::default();
    let needed = vec!["/repo/a".to_owned()];
    let mut diff = DiffStatsCache::default();
    diff.entries.insert(
        "/repo/a".to_owned(),
        DiffStatsCacheEntry {
            head_sha: Some("new".to_owned()),
            ..DiffStatsCacheEntry::default()
        },
    );

    assert!(
        cached_due_repo_keys(
            &cache,
            &needed,
            &diff,
            &BTreeSet::new(),
            &BTreeSet::new(),
            1_000
        )
        .is_none()
    );
}

#[test]
fn unsupported_reconcile_drops_state_and_marks_head_seen() {
    let mut cache = PrStateCache::default();
    cache.states.insert(
        "/repo/a".to_owned(),
        PrLink {
            state: WorktreePrState::Open,
            number: Some(91),
            ci: None,
            merge_sha: None,
        },
    );
    cache
        .path_repos
        .insert("/repo/a".to_owned(), "gh:github.com:org/repo".to_owned());
    cache
        .head_seen
        .insert("/repo/a".to_owned(), "old".to_owned());
    let needed = vec!["/repo/a".to_owned()];
    let mut diff = DiffStatsCache::default();
    diff.entries.insert(
        "/repo/a".to_owned(),
        DiffStatsCacheEntry {
            head_sha: Some("new".to_owned()),
            ..DiffStatsCacheEntry::default()
        },
    );
    let target_paths = BTreeSet::new();

    assert!(needs_target_reconcile(
        &cache,
        &needed,
        &diff,
        &target_paths
    ));

    let cache = reconcile_target_bookkeeping(cache, &needed, &diff, &target_paths, 1_000);

    assert!(!cache.states.contains_key("/repo/a"));
    assert_eq!(
        cache.path_repos.get("/repo/a").map(String::as_str),
        Some(UNSUPPORTED_REPO_KEY)
    );
    assert_eq!(
        cache.head_seen.get("/repo/a").map(String::as_str),
        Some("new")
    );
    assert_eq!(
        cache
            .repos
            .get(UNSUPPORTED_REPO_KEY)
            .map(|probe| probe.refreshed_at_ms),
        Some(1_000)
    );
    assert!(!needs_target_reconcile(
        &cache,
        &needed,
        &diff,
        &target_paths
    ));
}

#[test]
fn unsupported_cached_path_does_not_reassemble_on_head_nudge() {
    let mut cache = PrStateCache::default();
    cache
        .path_repos
        .insert("/repo/a".to_owned(), UNSUPPORTED_REPO_KEY.to_owned());
    cache.head_seen.insert("/repo/a".to_owned(), String::new());
    cache.repos.insert(
        UNSUPPORTED_REPO_KEY.to_owned(),
        RepoProbe {
            refreshed_at_ms: 1_000,
            ok: true,
            consecutive_failures: 0,
        },
    );
    let needed = vec!["/repo/a".to_owned()];
    let mut diff = DiffStatsCache::default();
    diff.entries.insert(
        "/repo/a".to_owned(),
        DiffStatsCacheEntry {
            head_sha: Some("new".to_owned()),
            ..DiffStatsCacheEntry::default()
        },
    );

    assert!(
        cached_due_repo_keys(
            &cache,
            &needed,
            &diff,
            &BTreeSet::new(),
            &BTreeSet::new(),
            1_000
        )
        .unwrap()
        .is_empty()
    );
    assert!(
        cached_due_repo_keys(
            &cache,
            &needed,
            &diff,
            &BTreeSet::new(),
            &BTreeSet::new(),
            1_001 + PR_STATE_TTL.as_millis() as u64
        )
        .unwrap()
        .contains(UNSUPPORTED_REPO_KEY)
    );
}

#[test]
fn repo_bookkeeping_prunes_stale_repo_stamps() {
    let mut prior = PrStateCache::default();
    prior
        .path_repos
        .insert("/repo/a".to_owned(), "gh:github.com:org/repo".to_owned());
    prior.repos.insert(
        "gh:github.com:org/repo".to_owned(),
        RepoProbe {
            refreshed_at_ms: 1_000,
            ok: true,
            consecutive_failures: 0,
        },
    );
    prior.repos.insert(
        "gh:github.com:old/repo".to_owned(),
        RepoProbe {
            refreshed_at_ms: 1_000,
            ok: true,
            consecutive_failures: 0,
        },
    );
    let groups = group_targets(vec![target("/repo/a", "a")]);
    let needed = vec!["/repo/a".to_owned()];

    let cache = probe_due_repos(
        &groups,
        &BTreeSet::new(),
        &prior,
        &needed,
        &DiffStatsCache::default(),
        2_000,
    );

    assert!(cache.repos.contains_key("gh:github.com:org/repo"));
    assert!(!cache.repos.contains_key("gh:github.com:old/repo"));
}

fn target(path: &str, branch: &str) -> Target {
    Target {
        path: path.to_owned(),
        branch: branch.to_owned(),
        remote: "https://github.com/org/repo.git".to_owned(),
        forge_cli: ForgeCli::Gh,
        repo_key: "gh:github.com:org/repo".to_owned(),
        repo_slug: Some("org/repo".to_owned()),
        worktree: PathBuf::from("/repo"),
        head_sha: Some("sha".to_owned()),
    }
}
