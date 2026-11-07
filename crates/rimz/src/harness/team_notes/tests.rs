use super::*;
use std::{fs, process::Command};

#[test]
fn resume_finds_the_newest_matching_run_across_both_layouts() {
    let temp = tempfile::tempdir().unwrap();
    let checkout = temp.path().join("checkout");
    fs::create_dir(&checkout).unwrap();
    let store = store(&temp.path().join("home"), &checkout);
    let legacy = store
        .paths()
        .team_notes_dir
        .join("20260101T000000Z-forge-checkout");
    let dated = store
        .paths()
        .team_notes_dir
        .join("20261107/002336Z-forge-checkout");
    for (run, at) in [
        (&legacy, "2026-01-01T00:00:00Z"),
        (&dated, "2026-11-07T00:23:36Z"),
    ] {
        fs::create_dir_all(run).unwrap();
        fs::write(
            run.join("run.json"),
            serde_json::to_vec(&serde_json::json!({
                "version": 1, "created_at": at, "team": "forge", "checkout": checkout,
            }))
            .unwrap(),
        )
        .unwrap();
    }
    fs::create_dir(checkout.join(".rimz")).unwrap();
    std::os::unix::fs::symlink(temp.path(), checkout.join(TEAM_LINK)).unwrap();
    assert_eq!(
        register(&store, &checkout, "forge", &team(), Launch::Resume).unwrap(),
        Some(dated)
    );
    fs::remove_file(checkout.join(TEAM_LINK)).unwrap();
    fs::remove_dir_all(store.paths().team_notes_dir.join("20261107")).unwrap();
    assert_eq!(
        register(&store, &checkout, "forge", &team(), Launch::Resume).unwrap(),
        Some(legacy)
    );
}

fn store(home: &Path, checkout: &Path) -> Store {
    let id = crate::WorkspaceId::from_project_root(checkout);
    Store::open(
        crate::StatePaths::under(id.clone(), home).unwrap(),
        crate::RuntimePaths::for_state_under(&crate::StatePaths::under(id, home).unwrap(), home),
    )
    .unwrap()
}

fn team() -> Team {
    toml::from_str("stages = ['Plan', 'Build']\nscratch-files = ['/extra.tmp']").unwrap()
}

fn git(checkout: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn registration_archives_a_linked_checkout_and_reuses_only_open_runs() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ],
    );
    let checkout = temp.path().join("checkout");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-qb",
            "feature",
            checkout.to_str().unwrap(),
        ],
    );
    let marker = crate::worktree::WorktreeMarker {
        version: 4,
        name: "checkout".into(),
        branch: "feature".into(),
        base_branch: Some("main".into()),
        from_pr: Some(42),
        base_ref: "main".into(),
        repo_root: repo.clone(),
        worktree_path: checkout.clone(),
        created_at: jiff::Timestamp::now(),
    };
    let git_dir = PathBuf::from(git(&checkout, &["rev-parse", "--absolute-git-dir"]));
    fs::write(
        git_dir.join("rimz-worktree.json"),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();
    let store = store(&temp.path().join("home"), &repo);
    let run = register(&store, &checkout, "forge", &team(), Launch::Fresh)
        .unwrap()
        .expect("staged team run");
    assert_eq!(
        run.parent().unwrap().parent().unwrap(),
        store.paths().team_notes_dir
    );
    assert_eq!(fs::read_link(checkout.join(".rimz/team")).unwrap(), run);
    let manifest = fs::read(run.join("run.json")).unwrap();
    let record: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
    assert_eq!(record["version"], 2);
    assert_eq!(record["kind"], "team");
    let at: jiff::Timestamp = record["created_at"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        run.parent().unwrap().file_name().unwrap(),
        at.strftime("%Y%m%d").to_string().as_str()
    );
    assert_eq!(record["team"], "forge");
    assert_eq!(record["checkout"], checkout.to_str().unwrap());
    assert_eq!(record["name"], "checkout");
    assert_eq!(record["branch"], "feature");
    assert_eq!(record["base_branch"], "main");
    assert_eq!(record["from_pr"], 42);
    assert_eq!(record["repo_root"], serde_json::json!(repo));
    assert!(
        record["created_at"]
            .as_str()
            .unwrap()
            .parse::<jiff::Timestamp>()
            .is_ok()
    );
    fs::write(checkout.join("extra.tmp"), "extra").unwrap();
    assert_eq!(
        git(
            &checkout,
            &["status", "--porcelain", "--untracked-files=all"]
        ),
        ""
    );
    assert_eq!(
        git(&checkout, &["check-ignore", ".rimz/team"]),
        ".rimz/team"
    );
    fs::write(run.join("blackboard.md"), "Stage: Build\n").unwrap();
    for launch in [Launch::Resume, Launch::Fresh] {
        assert_eq!(
            register(&store, &checkout, "forge", &team(), launch).unwrap(),
            Some(run.clone())
        );
    }
    let exclude = repo.join(".git/info/exclude");
    assert_eq!(
        fs::read_to_string(exclude)
            .unwrap()
            .lines()
            .filter(|line| line.contains(".rimz/team"))
            .collect::<Vec<_>>(),
        ["/.rimz/team"]
    );
    fs::write(run.join("blackboard.md"), "Stage: Done\n").unwrap();
    fs::write(run.join("plan-notes.md"), "archive").unwrap();
    assert_eq!(
        register(&store, &checkout, "forge", &team(), Launch::Resume).unwrap(),
        Some(run.clone())
    );
    let next = register(&store, &checkout, "forge", &team(), Launch::Fresh)
        .unwrap()
        .unwrap();
    assert_ne!(run, next);
    assert_eq!(fs::read(run.join("run.json")).unwrap(), manifest);
    assert_eq!(
        fs::read_to_string(run.join("blackboard.md")).unwrap(),
        "Stage: Done\n"
    );
    assert_eq!(
        fs::read_to_string(run.join("plan-notes.md")).unwrap(),
        "archive"
    );
    fs::write(
        next.join("blackboard.md"),
        "# Blackboard\n\n## Goal\nkeep this\n",
    )
    .unwrap();
    fs::write(next.join("plan-notes.md"), "original notes\n").unwrap();
    let snapshot = ["run.json", "blackboard.md", "plan-notes.md"]
        .map(|file| (file, fs::read(next.join(file)).unwrap()));
    let other = register(&store, &checkout, "mill", &team(), Launch::Fresh)
        .unwrap()
        .unwrap();
    assert_ne!(
        other, next,
        "another team must get its own run without a Stage"
    );
    for (file, bytes) in snapshot {
        assert_eq!(fs::read(next.join(file)).unwrap(), bytes);
    }
    assert_eq!(
        register(&store, &checkout, "mill", &team(), Launch::Fresh).unwrap(),
        Some(other.clone())
    );
    assert_eq!(
        register(&store, &checkout, "forge", &team(), Launch::Resume).unwrap(),
        Some(next.clone())
    );
    assert_eq!(
        register(&store, &checkout, "mill", &team(), Launch::Resume).unwrap(),
        Some(other.clone())
    );
    let mut linked = other;
    for unreadable in [false, true] {
        let manifest = linked.join("run.json");
        fs::remove_file(&manifest).unwrap();
        if unreadable {
            fs::create_dir(&manifest).unwrap();
        }
        let replacement = register(&store, &checkout, "mill", &team(), Launch::Resume)
            .unwrap()
            .unwrap();
        assert_ne!(
            replacement, linked,
            "a missing or unreadable manifest is not this team"
        );
        linked = replacement;
    }
    git(
        &repo,
        &["worktree", "remove", "--force", checkout.to_str().unwrap()],
    );
    assert!(run.join("blackboard.md").exists() && next.join("run.json").exists());
}

#[test]
fn subdirectory_registration_excludes_its_link_from_git_status() {
    let temp = tempfile::tempdir().unwrap();
    let checkout = temp.path().join("checkout");
    fs::create_dir(&checkout).unwrap();
    git(&checkout, &["init", "-q", "-b", "main"]);
    let store = store(&temp.path().join("home"), &checkout);
    for (subdir, pattern) in [
        ("crates/worker", "/crates/worker/.rimz/team"),
        (
            r"crates/[worker]*?\suffix",
            r"/crates/\[worker\]\*\?\\suffix/.rimz/team",
        ),
    ] {
        let cwd = checkout.join(subdir);
        fs::create_dir_all(&cwd).unwrap();
        for launch in [Launch::Fresh, Launch::Resume] {
            register(&store, &cwd, "forge", &team(), launch).unwrap();
        }
        assert_eq!(
            git(
                &checkout,
                &["status", "--porcelain", "--untracked-files=all"]
            ),
            "",
            "the subdirectory link must be excluded: {subdir}"
        );
        let exclude = fs::read_to_string(checkout.join(".git/info/exclude")).unwrap();
        for expected in ["/.rimz/team", pattern] {
            assert_eq!(
                exclude.lines().filter(|line| *line == expected).count(),
                1,
                "{expected}"
            );
        }
    }
}

#[test]
fn registration_restores_newest_matching_run_only_on_resume() {
    let temp = tempfile::tempdir().unwrap();
    let checkout = temp.path().join("checkout");
    fs::create_dir(&checkout).unwrap();
    let store = store(&temp.path().join("home"), &checkout);
    let old = register(&store, &checkout, "forge", &team(), Launch::Fresh)
        .unwrap()
        .unwrap();
    fs::write(old.join("blackboard.md"), "Stage: Done\n").unwrap();
    let newest = register(&store, &checkout, "forge", &team(), Launch::Fresh)
        .unwrap()
        .unwrap();
    let elsewhere = temp.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    register(&store, &elsewhere, "forge", &team(), Launch::Fresh).unwrap();
    let mill = register(&store, &checkout, "mill", &team(), Launch::Fresh)
        .unwrap()
        .unwrap();
    fs::write(mill.join("blackboard.md"), "mill's board").unwrap();
    let saved = ["run.json", "blackboard.md"].map(|file| fs::read(mill.join(file)).unwrap());
    assert_eq!(
        register(&store, &checkout, "forge", &team(), Launch::Resume).unwrap(),
        Some(newest.clone()),
        "resume must recover this team's newest run in this checkout"
    );
    assert_eq!(
        saved,
        ["run.json", "blackboard.md"].map(|file| fs::read(mill.join(file)).unwrap())
    );
    fs::remove_file(checkout.join(TEAM_LINK)).unwrap();
    let fresh = register(&store, &checkout, "forge", &team(), Launch::Fresh)
        .unwrap()
        .unwrap();
    assert_ne!(fresh, newest, "fresh must not restore an unlinked archive");
    let only_mill = register(&store, &elsewhere, "mill", &team(), Launch::Fresh)
        .unwrap()
        .unwrap();
    let new_team = register(&store, &elsewhere, "spot", &team(), Launch::Resume)
        .unwrap()
        .unwrap();
    assert_ne!(new_team, only_mill, "resume with no own run must open one");
}

#[test]
fn restored_links_keep_canonical_targets_under_an_aliased_home() {
    let temp = tempfile::tempdir().unwrap();
    let checkout = temp.path().join("checkout");
    let home = temp.path().join("home");
    let alias = temp.path().join("home-link");
    fs::create_dir(&checkout).unwrap();
    fs::create_dir(&home).unwrap();
    std::os::unix::fs::symlink(&home, &alias).unwrap();
    let state =
        crate::StatePaths::under(crate::WorkspaceId::from_project_root(&checkout), &alias).unwrap();
    let runtime = crate::RuntimePaths::for_state_under(&state, temp.path());
    let store = Store::open(state, runtime).unwrap();
    let run = register(&store, &checkout, "forge", &team(), Launch::Fresh)
        .unwrap()
        .unwrap();
    let link = checkout.join(TEAM_LINK);
    fs::remove_file(&link).unwrap();
    assert_eq!(
        register(&store, &checkout, "forge", &team(), Launch::Resume).unwrap(),
        Some(run.clone()),
        "a restored run must retain its canonical path under an aliased home"
    );
    assert_eq!(fs::read_link(link).unwrap(), run);
}

#[test]
fn registration_refuses_a_symlinked_rimz_directory() {
    let temp = tempfile::tempdir().unwrap();
    let checkout = temp.path().join("checkout");
    let shared = temp.path().join("shared-rimz");
    fs::create_dir(&checkout).unwrap();
    fs::create_dir(&shared).unwrap();
    let store = store(&temp.path().join("home"), &checkout);
    let kept = store.paths().team_notes_dir.join("kept");
    fs::create_dir_all(&kept).unwrap();
    fs::write(shared.join("config.toml"), "keep config").unwrap();
    fs::write(kept.join("blackboard.md"), "keep board").unwrap();
    std::os::unix::fs::symlink(&kept, shared.join("team")).unwrap();
    std::os::unix::fs::symlink(&shared, checkout.join(".rimz")).unwrap();
    let error = register(&store, &checkout, "forge", &team(), Launch::Fresh)
        .expect_err("a shared .rimz must not acquire a checkout-local team link")
        .to_string();
    assert!(
        error.contains(".worktreeinclude") && error.contains(".worktreelink"),
        "{error}"
    );
    assert_eq!(fs::read_link(shared.join("team")).unwrap(), kept);
    assert_eq!(
        fs::read_to_string(shared.join("config.toml")).unwrap(),
        "keep config"
    );
    assert_eq!(
        fs::read_to_string(kept.join("blackboard.md")).unwrap(),
        "keep board"
    );
    assert_eq!(
        fs::read_dir(&store.paths().team_notes_dir).unwrap().count(),
        1
    );
}

#[test]
fn registration_allows_a_home_nested_under_dot_rimz() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp.path().join(".rimz/home"), temp.path());
    assert!(
        register(&store, temp.path(), "forge", &team(), Launch::Fresh).is_ok(),
        "only .rimz itself being the home refuses, not an ancestor of the home"
    );
}

#[test]
fn registration_bounds_long_run_name_components() {
    let temp = tempfile::tempdir().unwrap();
    let checkout = temp.path().join(format!("{}é", "c".repeat(230)));
    fs::create_dir(&checkout).unwrap();
    let store = store(&temp.path().join("home"), temp.path());
    let name = format!("{}é", "t".repeat(230));
    let result = register(&store, &checkout, &name, &team(), Launch::Fresh);
    assert!(result.is_ok(), "long names must fit NAME_MAX: {result:?}");
    let run = result.unwrap().unwrap();
    assert!(
        run.file_name().unwrap().len() < 200,
        "leave space for collision suffixes"
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(run.join("run.json")).unwrap()).unwrap();
    assert_eq!(manifest["team"], name);
}

#[test]
fn registration_replaces_stale_and_foreign_links_without_touching_targets() {
    let temp = tempfile::tempdir().unwrap();
    let checkout = temp.path().join("checkout");
    fs::create_dir_all(checkout.join(".rimz")).unwrap();
    let store = store(&temp.path().join("home"), &checkout);
    let link = checkout.join(".rimz/team");
    std::os::unix::fs::symlink(temp.path().join("missing"), &link).unwrap();
    let first = register(&store, &checkout, "forge", &team(), Launch::Resume)
        .unwrap()
        .expect("replace dangling link");
    assert_eq!(link.canonicalize().unwrap(), first);
    fs::remove_file(&link).unwrap();
    let foreign = temp.path().join("foreign");
    fs::create_dir(&foreign).unwrap();
    fs::write(foreign.join("saved"), "keep").unwrap();
    std::os::unix::fs::symlink(&foreign, &link).unwrap();
    let second = register(&store, &checkout, "forge", &team(), Launch::Resume)
        .unwrap()
        .unwrap();
    assert_ne!(second, foreign);
    assert_eq!(fs::read_to_string(foreign.join("saved")).unwrap(), "keep");
}

#[test]
fn registration_refuses_user_files_and_the_rimz_home() {
    let temp = tempfile::tempdir().unwrap();
    let checkout = temp.path().join("checkout");
    fs::create_dir_all(checkout.join(".rimz/team")).unwrap();
    let store = store(&temp.path().join("home"), &checkout);
    fs::write(checkout.join(".rimz/team/saved"), "keep").unwrap();
    let error = register(&store, &checkout, "forge", &team(), Launch::Fresh)
        .expect_err("refuse a real directory")
        .to_string();
    assert!(
        error.contains(checkout.join(".rimz/team").to_str().unwrap()) && error.contains("move"),
        "{error}"
    );
    assert_eq!(
        fs::read_to_string(checkout.join(".rimz/team/saved")).unwrap(),
        "keep"
    );
    let home_checkout = temp.path().join("home-checkout");
    fs::create_dir(&home_checkout).unwrap();
    let home_store = super::tests::store(&home_checkout.join(".rimz"), &home_checkout);
    assert!(
        register(&home_store, &home_checkout, "forge", &team(), Launch::Fresh)
            .expect_err("refuse RimZ home")
            .to_string()
            .contains("RimZ home")
    );
}

#[test]
fn non_staged_registration_only_adds_declared_excludes() {
    let temp = tempfile::tempdir().unwrap();
    git(temp.path(), &["init", "-q"]);
    let store = store(&temp.path().join("home"), temp.path());
    let team: Team = toml::from_str("scratch-files = ['/extra.tmp']").unwrap();
    assert_eq!(
        register(&store, temp.path(), "plain", &team, Launch::Fresh).unwrap(),
        None
    );
    assert!(!temp.path().join(".rimz/team").exists());
    fs::write(temp.path().join("extra.tmp"), "extra").unwrap();
    assert_eq!(
        git(temp.path(), &["check-ignore", "extra.tmp"]),
        "extra.tmp"
    );
}
