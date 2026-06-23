use super::*;
use std::path::PathBuf;

struct GitFixture {
    dir: tempfile::TempDir,
    initialized: bool,
}

impl GitFixture {
    fn init(args: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let initialized = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(args)
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        let fixture = Self { dir, initialized };
        if initialized {
            let _ = fixture.git(&["config", "user.email", "t@example.com"]);
            let _ = fixture.git(&["config", "user.name", "t"]);
        }
        fixture
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn path_str(&self) -> &str {
        self.path().to_str().unwrap()
    }

    fn git(&self, args: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(self.path())
            .args(args)
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.path().join(name), body).unwrap();
    }
}

fn runtime_for(path: &Path) -> (tempfile::TempDir, crate::RuntimePaths) {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        crate::RuntimePaths::under(crate::ids::WorkspaceId::from_project_root(path), dir.path())
            .unwrap();
    runtime.ensure_dirs().unwrap();
    (dir, runtime)
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
fn parse_status_entries_reads_clean_dirty_and_untracked() {
    let clean = parse_status_entries("", |_| unreachable!("no entries to count"));
    assert!(clean.clean);
    assert_eq!(clean.untracked_added, 0);

    // One modified + two untracked entries; only the `??` paths feed the
    // line counter, and any entry at all reads dirty.
    let status =
        parse_status_entries(
            " M src/lib.rs\0?? notes.txt\0?? docs/new.md\0",
            |path| match path {
                "notes.txt" => 3,
                "docs/new.md" => 4,
                other => panic!("unexpected untracked path {other}"),
            },
        );
    assert!(!status.clean);
    assert_eq!(status.untracked_added, 7);

    // A rename entry — staged (`R `) or detected in the worktree column
    // (` R`) — carries its source path as a second NUL token; it must not
    // read as an entry (or an untracked path) of its own.
    let renamed = parse_status_entries(
        "R  new.rs\0old.rs\0 R moved.rs\0was.rs\0?? x.txt\0",
        |path| {
            assert_eq!(path, "x.txt");
            1
        },
    );

    assert!(!renamed.clean);
    assert_eq!(renamed.untracked_added, 1);
}

#[test]
fn count_added_lines_counts_text_and_skips_binary() {
    assert_eq!(count_added_lines(b""), 0);
    assert_eq!(count_added_lines(b"a\nb\n"), 2);
    // A trailing partial line still counts, the way numstat counts one.
    assert_eq!(count_added_lines(b"a\nb"), 2);
    // A NUL reads as binary — mirroring numstat's `-` cells, count nothing.
    assert_eq!(count_added_lines(b"a\x00b\n"), 0);
}

#[test]
fn untracked_added_lines_spends_a_shared_read_budget() {
    let dir = tempfile::tempdir().unwrap();
    let small = dir.path().join("small.txt");
    let big = dir.path().join("big.txt");
    std::fs::write(&small, "a\nb\n").unwrap();
    std::fs::write(&big, "c\nd\ne\n").unwrap();

    // The first file fits and spends its bytes; the second overflows what's
    // left, counts nothing (its status entry still dirties the tree), and
    // leaves the remainder intact.
    let mut budget = 6;
    assert_eq!(untracked_added_lines(&small, &mut budget), 2);
    assert_eq!(budget, 2);
    assert_eq!(untracked_added_lines(&big, &mut budget), 0);
    assert_eq!(budget, 2);
}

#[test]
fn worktree_branch_reads_live_checkout() {
    let repo = GitFixture::init(&["init", "-q"]);
    if !repo.initialized {
        // No git on PATH (or init failed); the helper degrades to None,
        // which is the documented fallback. Nothing to assert.
        assert_eq!(worktree_branch(repo.path()), None);
        return;
    }
    let _ = repo.git(&["checkout", "-q", "-b", "feature-migration"]);
    repo.write("f", "x");
    let _ = repo.git(&["add", "f"]);
    let _ = repo.git(&["commit", "-q", "-m", "init"]);

    assert_eq!(
        worktree_branch(repo.path()).as_deref(),
        Some("feature-migration"),
        "the live branch is read from the worktree, overriding any pinned label"
    );
    // A non-repository path has no branch to track.
    let plain = tempfile::tempdir().unwrap();
    assert_eq!(worktree_branch(plain.path()), None);
}

#[test]
fn worktree_diff_stats_total_committed_staged_and_unstaged_over_trunk() {
    let repo = GitFixture::init(&["init", "-q", "-b", "main"]);
    // `-b main` needs Git >= 2.28; an older git fails init and the helper
    // degrades to None, which is the documented fallback.
    if !repo.initialized {
        assert_eq!(
            refresh_entry(repo.path_str(), None, DueFacts::all(), None).stats(),
            None
        );
        return;
    }

    // Fork point on `main`: a three-line tracked file.
    repo.write("base.txt", "a\nb\nc\n");
    let _ = repo.git(&["add", "base.txt"]);
    let _ = repo.git(&["commit", "-q", "-m", "base"]);
    let _ = repo.git(&["branch", "feature-migration"]);

    // `main` advances *after* the fork — a merge-base diff must ignore this,
    // so it never shows up as the worktree's own churn.
    repo.write("base.txt", "a\nB\nc\n");
    let _ = repo.git(&["commit", "-aqm", "trunk moves on"]);

    let _ = repo.git(&["checkout", "-q", "feature-migration"]);
    // Committed on the branch: a new two-line file.
    repo.write("feat.txt", "x\ny\n");
    let _ = repo.git(&["add", "feat.txt"]);
    let _ = repo.git(&["commit", "-q", "-m", "feature work"]);
    // Staged but uncommitted: a new one-line file.
    repo.write("staged.txt", "s\n");
    let _ = repo.git(&["add", "staged.txt"]);
    // Unstaged: one more line appended to a tracked file.
    repo.write("base.txt", "a\nb\nc\nd\n");

    let entry = refresh_entry(repo.path_str(), None, DueFacts::all(), None);
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
    // Staged + unstaged change reads dirty — the landed markers stay off.
    assert_eq!(entry.clean, Some(false));

    // A non-repository path has nothing to diff, count, or status-read.
    let plain = tempfile::tempdir().unwrap();
    let plain_entry = refresh_entry(plain.path().to_str().unwrap(), None, DueFacts::all(), None);
    assert_eq!(plain_entry.stats(), None);
    assert_eq!(plain_entry.commits, None);
    assert_eq!(plain_entry.behind, None);
    assert_eq!(plain_entry.trunk, None);
    assert_eq!(plain_entry.clean, None);
}

#[test]
fn focused_diff_stats_refreshes_local_facts_before_commit_facts() {
    let repo = GitFixture::init(&["init", "-q", "-b", "main"]);
    if !repo.initialized {
        return;
    }
    repo.write("base.txt", "base\n");
    let _ = repo.git(&["add", "base.txt"]);
    let _ = repo.git(&["commit", "-q", "-m", "base"]);
    repo.write("base.txt", "base\nedit\n");

    let (_runtime_dir, runtime) = runtime_for(repo.path());
    let cache_path = runtime.root.join("diff-stats.json");
    let path = repo.path_str().to_owned();
    let mut cache = DiffStatsCache::default();
    cache.entries.insert(
        path.clone(),
        DiffStatsCacheEntry {
            refreshed_at_ms: 1_000,
            commit_refreshed_at_ms: Some(1_000),
            added: Some(0),
            removed: Some(0),
            commits: Some(9),
            behind: Some(8),
            trunk: Some("main".to_owned()),
            branch: Some("main".to_owned()),
            clean: Some(true),
            landed: Some(false),
        },
    );
    atomic::write_temp_then_rename_cache(&cache_path, &cache).unwrap();

    let refreshed = refresh_diff_stats(
        &cache_path,
        &runtime,
        std::slice::from_ref(&path),
        &BTreeSet::from([path.clone()]),
        &BTreeSet::new(),
        1_000 + DIFF_STATS_FOCUSED_LOCAL_TTL.as_millis() as u64 + 1,
        None,
    );
    let entry = refreshed.entries.get(&path).unwrap();

    assert_eq!(
        entry.stats(),
        Some(DiffStats {
            added: 1,
            removed: 0
        })
    );
    assert_eq!(entry.clean, Some(false));
    assert_eq!(
        entry.commits,
        Some(9),
        "commit facts stay cached on the 3s pass"
    );
    assert_eq!(entry.behind, Some(8));
    assert_eq!(entry.landed, Some(false));
    assert_eq!(entry.commit_refreshed_at_ms, Some(1_000));
}

#[test]
fn non_focused_diff_stats_refreshes_local_and_commit_facts_together() {
    let repo = GitFixture::init(&["init", "-q", "-b", "main"]);
    if !repo.initialized {
        return;
    }
    repo.write("base.txt", "base\n");
    let _ = repo.git(&["add", "base.txt"]);
    let _ = repo.git(&["commit", "-q", "-m", "base"]);
    repo.write("base.txt", "base\nedit\n");

    let (_runtime_dir, runtime) = runtime_for(repo.path());
    let cache_path = runtime.root.join("diff-stats.json");
    let path = repo.path_str().to_owned();
    let now_ms = 1_000 + DIFF_STATS_TTL.as_millis() as u64 + 1;
    let mut cache = DiffStatsCache::default();
    cache.entries.insert(
        path.clone(),
        DiffStatsCacheEntry {
            refreshed_at_ms: 1_000,
            commit_refreshed_at_ms: Some(now_ms),
            added: Some(0),
            removed: Some(0),
            commits: Some(9),
            behind: Some(8),
            trunk: Some("main".to_owned()),
            branch: Some("main".to_owned()),
            clean: Some(true),
            landed: Some(false),
        },
    );
    atomic::write_temp_then_rename_cache(&cache_path, &cache).unwrap();

    let refreshed = refresh_diff_stats(
        &cache_path,
        &runtime,
        std::slice::from_ref(&path),
        &BTreeSet::new(),
        &BTreeSet::from([path.clone()]),
        now_ms,
        None,
    );
    let entry = refreshed.entries.get(&path).unwrap();

    assert_eq!(
        entry.stats(),
        Some(DiffStats {
            added: 1,
            removed: 0
        })
    );
    assert_eq!(
        entry.commits,
        Some(0),
        "equal non-focused TTLs force commit refresh with local refresh"
    );
    assert_eq!(entry.behind, Some(0));
    assert_eq!(entry.landed, Some(true));
    assert!(
        entry
            .commit_refreshed_at_ms
            .is_some_and(|stamp| stamp > now_ms),
        "commit facts stamp at completion, not at stale-check time"
    );
}

#[test]
fn diff_stats_refresh_stamps_fact_groups_at_completion() {
    let repo = GitFixture::init(&["init", "-q", "-b", "main"]);
    if !repo.initialized {
        return;
    }
    repo.write("base.txt", "base\n");
    let _ = repo.git(&["add", "base.txt"]);
    let _ = repo.git(&["commit", "-q", "-m", "base"]);

    let entry = refresh_entry(repo.path_str(), None, DueFacts::all(), None);

    assert!(entry.refreshed_at_ms > 0);
    assert!(entry.commit_refreshed_at_ms.is_some_and(|stamp| stamp > 0));
}

#[test]
fn worktree_status_folds_untracked_into_churn_and_reads_clean() {
    let repo = GitFixture::init(&["init", "-q", "-b", "main"]);
    // `-b main` needs Git >= 2.28; an older git fails init and the helper
    // degrades to None, which is the documented fallback.
    if !repo.initialized {
        assert_eq!(
            refresh_entry(repo.path_str(), None, DueFacts::all(), None).clean,
            None
        );
        return;
    }
    repo.write("base.txt", "a\nb\n");
    let _ = repo.git(&["add", "base.txt"]);
    let _ = repo.git(&["commit", "-q", "-m", "base"]);

    // A pristine checkout at the trunk tip: proven clean, zero churn — the
    // exact reading the `≡` marker requires.
    let entry = refresh_entry(repo.path_str(), None, DueFacts::all(), None);
    assert_eq!(entry.clean, Some(true));
    assert_eq!(entry.stats(), Some(DiffStats::default()));

    // An untracked two-line file nested in an untracked directory: invisible
    // to `git diff`, so the status probe must flag the tree dirty and fold
    // the lines into `+` (`--untracked-files=all` reaches inside the dir).
    std::fs::create_dir_all(repo.path().join("sub")).unwrap();
    repo.write("sub/notes.txt", "n1\nn2\n");
    let entry = refresh_entry(repo.path_str(), None, DueFacts::all(), None);
    assert_eq!(entry.clean, Some(false));
    assert_eq!(
        entry.stats(),
        Some(DiffStats {
            added: 2,
            removed: 0,
        }),
        "untracked lines count as churn"
    );

    // An untracked binary contributes no lines but still dirties the tree.
    std::fs::write(repo.path().join("blob.bin"), b"\x00\x01\x02").unwrap();
    let entry = refresh_entry(repo.path_str(), None, DueFacts::all(), None);
    assert_eq!(entry.clean, Some(false));
    assert_eq!(
        entry.stats(),
        Some(DiffStats {
            added: 2,
            removed: 0,
        }),
        "a binary blob dirties the tree without inventing a line count"
    );
}

#[test]
fn trunk_ladder_prefers_a_configured_branch_that_resolves() {
    let repo = GitFixture::init(&["init", "-q", "-b", "main"]);
    if !repo.initialized {
        assert_eq!(trunk_ref(repo.path(), Some("develop")), None);
        return;
    }
    repo.write("f", "x");
    let _ = repo.git(&["add", "f"]);
    let _ = repo.git(&["commit", "-q", "-m", "init"]);
    let _ = repo.git(&["branch", "develop"]);

    // The configured branch exists here, so it wins over `main`.
    assert_eq!(
        trunk_ref(repo.path(), Some("develop")).as_deref(),
        Some("develop")
    );
    // A machine-wide preference this repo lacks falls through to detection
    // rather than losing the repo's stats.
    assert_eq!(
        trunk_ref(repo.path(), Some("absent")).as_deref(),
        Some("main")
    );
    // An option-shaped name is never handed to git; detection stands alone.
    assert_eq!(
        trunk_ref(repo.path(), Some("--help")).as_deref(),
        Some("main")
    );
    assert_eq!(trunk_ref(repo.path(), None).as_deref(), Some("main"));
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

#[test]
fn child_repo_enumeration_accepts_git_dir_and_git_file_children() {
    // `.git` may be a directory (a normal clone) or a pointer file (a linked
    // worktree or submodule checkout); both mint pods. Non-repo children and
    // plain files never qualify, and the result is sorted so the cache and
    // the reducer see a stable set.
    let room = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(room.path().join("clone/.git")).unwrap();
    std::fs::create_dir_all(room.path().join("linked-wt")).unwrap();
    std::fs::write(room.path().join("linked-wt/.git"), "gitdir: /elsewhere").unwrap();
    std::fs::create_dir_all(room.path().join("notes")).unwrap();
    std::fs::write(room.path().join("README.md"), "hi").unwrap();

    let roots = list_child_repo_roots(room.path());
    assert_eq!(
        roots,
        vec![room.path().join("clone"), room.path().join("linked-wt")]
    );
    // Best-effort: an unreadable room root yields no child pods.
    assert!(list_child_repo_roots(Path::new("/nonexistent-rimz-room")).is_empty());
}

#[test]
fn group_roots_dispatch_follows_the_root_class() {
    // A directory room scans its children; a marker room without `.git` at
    // the root does the same (repo semantics need `.git` at the root).
    let room = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(room.path().join("app/.git")).unwrap();
    std::fs::write(room.path().join("Cargo.toml"), "[workspace]").unwrap();

    let expected = vec![room.path().join("app")];
    assert_eq!(
        list_group_roots(room.path(), RootClass::Directory),
        expected
    );
    assert_eq!(list_group_roots(room.path(), RootClass::Marker), expected);
}
