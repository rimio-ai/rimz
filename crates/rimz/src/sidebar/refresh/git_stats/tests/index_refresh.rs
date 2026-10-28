use super::*;

#[test]
fn index_refresh_requires_stale_entries_and_a_sixty_second_cooldown() {
    let now = Instant::now();
    let cases = [
        (0, None, false),
        (127, None, false),
        (128, None, true),
        (129, Some(now - Duration::from_millis(59_999)), false),
        (128, Some(now - Duration::from_secs(60)), true),
        (128, Some(now + Duration::from_secs(1)), false),
    ];
    let actual: Vec<_> = cases
        .iter()
        .map(|&(stale_entries, last_locked_at, _)| {
            IndexRefreshState {
                stale_entries,
                last_locked_at,
            }
            .may_lock(now)
        })
        .collect();
    assert_eq!(actual, cases.map(|(_, _, expected)| expected));
}

#[test]
fn trace2_scan_count_requires_the_index_refresh_data_event() {
    let cases: &[(&[u8], u64)] = &[
        (b"", 0),
        (b"warning: ordinary git stderr\n", 0),
        (br#"{"event":"data","category":"index","key":"refresh/sum_scan","value":"128"}"#, 128),
        (br#"{"event":"data","category":"other","key":"refresh/sum_scan","value":"999"}"#, 0),
        (br#"{"event":"data","category":"index","key":"refresh/sum_lstat","value":"999"}"#, 0),
        (br#"{"event":"data","category":"index","key":"refresh/sum_scan","value":"bad"}"#, 0),
        (br#"{"event":"data","category":"index","key":"refresh/sum_scan","value":"0"}"#, 0),
        (b"not json\n{\"event\":\"data\",\"category\":\"index\",\"key\":\"refresh/sum_scan\",\"value\":\"129\"}\n", 129),
    ];
    let actual: Vec<_> = cases
        .iter()
        .map(|(stderr, _)| index_scan_count(stderr))
        .collect();
    assert_eq!(
        actual,
        cases.iter().map(|(_, count)| *count).collect::<Vec<_>>()
    );
}

#[test]
fn status_counts_only_worktree_changes_not_staged_or_untracked_paths() {
    let cases = [
        (" M f\0MM g\0M  h\0?? u\0", 2),
        (" D f\0 T g\0R  new\0 M source\0 R moved\0old\0", 2),
        (
            " D deleted\0UU unmerged\0 A intent-to-add\0 C copied\0original\0",
            2,
        ),
        ("", 0),
    ];
    let actual: Vec<_> = cases
        .iter()
        .map(|(output, _)| parse_status_entries(output, |_| 0).scanned_changes)
        .collect();
    assert_eq!(actual, cases.map(|(_, modified)| modified));
}

fn tracked_repo(count: usize) -> GitFixture {
    let repo = GitFixture::init(&["init", "-q", "-b", "main"]);
    assert!(repo.initialized, "git fixture initialization failed");
    for index in 0..count {
        repo.write(&format!("f{index:03}.txt"), "base\n");
    }
    assert!(repo.git(&["add", "."]));
    assert!(repo.git(&["commit", "-qm", "base"]));
    assert!(repo.git(&["update-index", "-q", "--refresh"]));
    repo
}

fn stale_files(repo: &GitFixture, count: usize, hours: u64) {
    let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000 - hours * 3600);
    for index in 0..count {
        std::fs::File::options()
            .write(true)
            .open(repo.path().join(format!("f{index:03}.txt")))
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
    }
}

fn index_path(repo: &GitFixture) -> PathBuf {
    git_path_from_rev_parse(
        repo.path(),
        &git_line(repo.path(), &["rev-parse", "--git-path", "index"]).unwrap(),
    )
}

fn index_snapshot(repo: &GitFixture) -> (Vec<u8>, SystemTime) {
    let path = index_path(repo);
    (
        std::fs::read(&path).unwrap(),
        std::fs::metadata(path).unwrap().modified().unwrap(),
    )
}

fn pass(
    repo: &GitFixture,
    runtime: &crate::RuntimePaths,
    state: &mut GitRefreshState,
    kind: GitPass,
) -> DiffStatsCacheEntry {
    refresh_diff_stats(
        &runtime.diff_stats_path(),
        runtime,
        &[repo.path_str().to_owned()],
        &BTreeSet::new(),
        &BTreeSet::new(),
        &BTreeSet::new(),
        u64::MAX,
        Some("main"),
        kind,
        state,
    )
    .entries
    .remove(repo.path_str())
    .unwrap()
}

fn scan_count(repo: &GitFixture) -> u64 {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args([
            "--no-optional-locks",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
        ])
        .env("GIT_TRACE2_EVENT", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "git status: {output:?}");
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|event| event["category"] == "index" && event["key"] == "refresh/sum_scan")
        .and_then(|event| event["value"].as_str().and_then(|value| value.parse().ok()))
        .expect("git trace2 refresh/sum_scan event")
}

#[test]
fn host_pass_preserves_the_index_below_the_stale_threshold() {
    let mut actual = Vec::new();
    for count in [1, 127] {
        for dirty in [false, true] {
            let repo = tracked_repo(count);
            let (_dir, runtime) = runtime_for(repo.path());
            if dirty {
                repo.write("f000.txt", "base\nedit\n");
            }
            stale_files(&repo, count, 1);
            let before = index_snapshot(&repo);
            let facts = pass(
                &repo,
                &runtime,
                &mut GitRefreshState::default(),
                GitPass::Host,
            );
            actual.push((
                count,
                dirty,
                index_snapshot(&repo) == before,
                facts.clean,
                facts.added,
                facts.removed,
            ));
        }
    }
    assert_eq!(
        actual,
        [
            (1, false, true, Some(true), Some(0), Some(0)),
            (1, true, true, Some(false), Some(1), Some(0)),
            (127, false, true, Some(true), Some(0), Some(0)),
            (127, true, true, Some(false), Some(1), Some(0)),
        ]
    );

    let repo = tracked_repo(1);
    let source = tracked_repo(1);
    assert!(repo.git(&[
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "add",
        "-q",
        source.path_str(),
        "sub",
    ]));
    assert!(repo.git(&["commit", "-qm", "submodule"]));
    let submodule = repo.path().join("sub");
    std::fs::File::options()
        .write(true)
        .open(submodule.join("f000.txt"))
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000))
        .unwrap();
    let submodule_index = git_path_from_rev_parse(
        &submodule,
        &git_line(&submodule, &["rev-parse", "--git-path", "index"]).unwrap(),
    );
    let before = std::fs::read(&submodule_index).unwrap();
    #[cfg(unix)]
    let before_inode = {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(&submodule_index).unwrap().ino()
    };
    let (_dir, runtime) = runtime_for(repo.path());
    let facts = pass(
        &repo,
        &runtime,
        &mut GitRefreshState::default(),
        GitPass::Host,
    );
    assert_eq!(facts.clean, Some(true));
    assert_eq!(std::fs::read(&submodule_index).unwrap(), before);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            std::fs::metadata(&submodule_index).unwrap().ino(),
            before_inode
        );
    }
}

#[test]
fn host_refreshes_only_after_a_stale_status_and_waits_out_the_cooldown() {
    let repo = tracked_repo(128);
    let (_dir, runtime) = runtime_for(repo.path());
    let mut state = GitRefreshState::default();
    stale_files(&repo, 128, 1);
    assert_eq!(scan_count(&repo), 128);
    let before = index_snapshot(&repo);
    let first = pass(&repo, &runtime, &mut state, GitPass::Host);
    let first_unchanged = index_snapshot(&repo) == before;
    let second = pass(&repo, &runtime, &mut state, GitPass::Host);
    let second_refreshed = index_snapshot(&repo) != before && scan_count(&repo) == 0;
    stale_files(&repo, 128, 2);
    let restaled = index_snapshot(&repo);
    pass(&repo, &runtime, &mut state, GitPass::Host);
    pass(&repo, &runtime, &mut state, GitPass::Host);
    let cooldown_unchanged = index_snapshot(&repo) == restaled;
    assert_eq!(
        (
            first_unchanged,
            second_refreshed,
            cooldown_unchanged,
            first.clean,
            second.clean
        ),
        (true, true, true, Some(true), Some(true))
    );
    state
        .index_refresh
        .get_mut(repo.path_str())
        .unwrap()
        .last_locked_at = Some(Instant::now() - Duration::from_secs(60));
    pass(&repo, &runtime, &mut state, GitPass::Host);
    assert_ne!(index_snapshot(&repo), restaled);
    assert_eq!(scan_count(&repo), 0);
}

#[test]
fn a_contended_index_refresh_returns_facts_and_consumes_its_lock_window() {
    let mut actual = Vec::new();
    for dirty in [false, true] {
        let repo = tracked_repo(129);
        let (_dir, runtime) = runtime_for(repo.path());
        let mut state = GitRefreshState::default();
        if dirty {
            repo.write("f000.txt", "base\nedit\n");
        }
        stale_files(&repo, 129, 1);
        pass(&repo, &runtime, &mut state, GitPass::Host);
        let lock = index_path(&repo).with_extension("lock");
        std::fs::write(&lock, "held by agent").unwrap();
        let before = index_snapshot(&repo);
        let facts = pass(&repo, &runtime, &mut state, GitPass::Host);
        let unchanged = index_snapshot(&repo) == before;
        let attempted = state
            .index_refresh
            .get(repo.path_str())
            .is_some_and(|state| state.last_locked_at.is_some());
        std::fs::remove_file(lock).unwrap();
        refresh_diff_stats(
            &runtime.diff_stats_path(),
            &runtime,
            &[],
            &BTreeSet::new(),
            &BTreeSet::new(),
            &BTreeSet::new(),
            u64::MAX,
            Some("main"),
            GitPass::Host,
            &mut state,
        );
        pass(&repo, &runtime, &mut state, GitPass::Host);
        pass(&repo, &runtime, &mut state, GitPass::Host);
        actual.push((
            dirty,
            unchanged,
            attempted,
            index_snapshot(&repo) == before,
            facts.clean,
            facts.added,
        ));
    }
    assert_eq!(
        actual,
        [
            (false, true, true, true, Some(true), Some(0)),
            (true, true, true, true, Some(false), Some(1)),
        ]
    );
}

#[test]
fn unused_index_refresh_state_expires_only_after_its_lock_window() {
    let now = Instant::now();
    let mut state = GitRefreshState {
        index_refresh: HashMap::from([
            ("/needed".to_owned(), IndexRefreshState::default()),
            ("/unlocked".to_owned(), IndexRefreshState::default()),
            (
                "/recent".to_owned(),
                IndexRefreshState {
                    stale_entries: 128,
                    last_locked_at: Some(now),
                },
            ),
            (
                "/expired".to_owned(),
                IndexRefreshState {
                    stale_entries: 128,
                    last_locked_at: Some(now - INDEX_REFRESH_INTERVAL),
                },
            ),
        ]),
        ..Default::default()
    };
    state.retain_needed(&["/needed".to_owned()]);
    assert_eq!(
        state
            .index_refresh
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["/needed", "/recent"])
    );
}

#[test]
fn modified_files_do_not_trigger_refresh_and_one_shot_never_locks() {
    let mut actual = Vec::new();
    for kind in [GitPass::Host, GitPass::OneShot] {
        let repo = tracked_repo(129);
        let (_dir, runtime) = runtime_for(repo.path());
        let mut state = GitRefreshState::default();
        for index in 0..129 {
            repo.write(&format!("f{index:03}.txt"), "base\nedit\n");
        }
        let before = index_snapshot(&repo);
        pass(&repo, &runtime, &mut state, kind);
        let facts = pass(&repo, &runtime, &mut state, kind);
        let attempted = state
            .index_refresh
            .get(repo.path_str())
            .is_some_and(|state| state.last_locked_at.is_some());
        actual.push((
            index_snapshot(&repo) == before,
            attempted,
            facts.clean,
            facts.added,
        ));
    }
    let repo = tracked_repo(128);
    let (_dir, runtime) = runtime_for(repo.path());
    let mut state = GitRefreshState::default();
    stale_files(&repo, 128, 1);
    let before = index_snapshot(&repo);
    pass(&repo, &runtime, &mut state, GitPass::Host);
    let facts = pass(&repo, &runtime, &mut state, GitPass::OneShot);
    let attempted = state
        .index_refresh
        .get(repo.path_str())
        .is_some_and(|state| state.last_locked_at.is_some());
    actual.push((
        index_snapshot(&repo) == before,
        attempted,
        facts.clean,
        facts.added,
    ));
    assert_eq!(
        actual,
        [
            (true, false, Some(false), Some(129)),
            (true, false, Some(false), Some(129)),
            (true, false, Some(true), Some(0)),
        ]
    );
}

#[test]
fn intent_to_add_paths_and_worktree_renames_never_trigger_refresh() {
    let mut actual = Vec::new();
    for renamed in [false, true] {
        let repo = tracked_repo(128);
        let (_dir, runtime) = runtime_for(repo.path());
        let mut state = GitRefreshState::default();
        let added: Vec<String> = (0..128).map(|index| format!("n{index:03}.txt")).collect();
        for (index, path) in added.iter().enumerate() {
            let source = repo.path().join(format!("f{index:03}.txt"));
            if renamed {
                std::fs::rename(source, repo.path().join(path)).unwrap();
            } else {
                repo.write(path, "new\n");
            }
        }
        let mut args = vec!["add", "-N", "--"];
        args.extend(added.iter().map(String::as_str));
        assert!(repo.git(&args));
        let scanned = scan_count(&repo);
        let before = index_snapshot(&repo);
        pass(&repo, &runtime, &mut state, GitPass::Host);
        pass(&repo, &runtime, &mut state, GitPass::Host);
        let attempted = state
            .index_refresh
            .get(repo.path_str())
            .is_some_and(|state| state.last_locked_at.is_some());
        actual.push((scanned, index_snapshot(&repo) == before, attempted));
    }
    assert_eq!(actual, [(128, true, false), (128, true, false)]);
}
