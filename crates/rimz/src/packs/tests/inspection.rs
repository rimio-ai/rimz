use super::git_sources::git;
use super::*;
use std::time::{Duration, SystemTime};

fn fixture(root: &Path, with_submodule: bool) -> (PathBuf, PathBuf, String) {
    let home = root.join("home");
    let repo = source(root, "repo", &[("crew", ".")]);
    write(&repo.join("tracked.txt"), "base\n");
    write(&repo.join(".gitignore"), "ignored\n");
    git(&repo, &["init", "-b", "main"]);
    if with_submodule {
        let submodule = root.join("submodule");
        fs::create_dir(&submodule).unwrap();
        write(&submodule.join("contents"), "nested\n");
        git(&submodule, &["init", "-b", "main"]);
        git(&submodule, &["add", "."]);
        git(&submodule, &["commit", "-m", "submodule"]);
        git(
            &repo,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                submodule.to_str().unwrap(),
                "nested",
            ],
        );
    }
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "fixture"]);
    let InstallSource::Git(mut spec) = url::Url::from_directory_path(&repo)
        .unwrap()
        .as_str()
        .parse()
        .unwrap()
    else {
        panic!("file URL must be a Git source");
    };
    if with_submodule {
        spec.reference = Some("main".into());
    }
    transaction::Transaction::stage(&home, &spec)
        .unwrap()
        .commit()
        .unwrap();
    let record = Record::read(&home).unwrap();
    let checkout =
        crate::packs::layout::checkout(&paths::packs_dir(&home), &record.sources[0]).unwrap();
    if with_submodule {
        git(
            &checkout,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "update",
                "--init",
            ],
        );
    }
    let commit = record.sources[0].commit.clone().unwrap();
    let linked = source(root, "linked", &[("linked", ".")]);
    install(&home, &linked).unwrap();
    write(&linked.join("rimz-packs.toml"), "not a manifest");
    (home, checkout, commit)
}

fn change_checkout(root: &Path, checkout: &Path, commit: &str, case: &str) {
    match case {
        "clean" => {}
        "modified" => write(&checkout.join("tracked.txt"), "modified\n"),
        "deleted" => fs::remove_file(checkout.join("tracked.txt")).unwrap(),
        "untracked" => write(&checkout.join("untracked dir/new\nfile.txt"), "new\n"),
        "ignored" => write(&checkout.join("ignored"), "ignored\n"),
        "head" => {
            git(checkout, &["commit", "--allow-empty", "-m", "moved HEAD"]);
        }
        "renamed" => {
            git(checkout, &["mv", "tracked.txt", "renamed.txt"]);
        }
        "conflict" => {
            git(checkout, &["switch", "-c", "side"]);
            write(&checkout.join("tracked.txt"), "side\n");
            git(checkout, &["commit", "-am", "side"]);
            git(checkout, &["switch", "--detach", commit]);
            write(&checkout.join("tracked.txt"), "other\n");
            git(checkout, &["commit", "-am", "other"]);
            let merge = std::process::Command::new("git")
                .arg("-C")
                .arg(checkout)
                .args([
                    "-c",
                    "user.name=Pack Test",
                    "-c",
                    "user.email=packs@example.test",
                ])
                .args(["merge", "side"])
                .output()
                .unwrap();
            assert_eq!(merge.status.code(), Some(1), "{merge:?}");
            assert!(
                String::from_utf8_lossy(&merge.stdout).contains("CONFLICT"),
                "{merge:?}"
            );
        }
        "missing-git" => {
            git(root, &["init", "-b", "parent"]);
            write(&root.join("parent.txt"), "parent\n");
            git(root, &["add", "parent.txt"]);
            git(root, &["commit", "-m", "parent"]);
            fs::remove_dir_all(checkout.join(".git")).unwrap();
        }
        "read-only" => {
            for path in ["tracked.txt", "nested/contents"] {
                fs::File::open(checkout.join(path))
                    .unwrap()
                    .set_times(
                        fs::FileTimes::new()
                            .set_modified(SystemTime::now() - Duration::from_secs(3600)),
                    )
                    .unwrap();
            }
        }
        _ => unreachable!("known test case"),
    }
}

#[test]
fn checkout_inspection_reports_git_changes_without_writing_or_inspecting_links() {
    let cases = [
        "clean",
        "modified",
        "deleted",
        "untracked",
        "ignored",
        "head",
        "renamed",
        "conflict",
        "missing-git",
        "read-only",
    ];
    let mut results = Vec::new();
    for case in cases {
        let temp = tempfile::tempdir().unwrap();
        let (home, checkout, commit) = fixture(temp.path(), case == "read-only");
        change_checkout(temp.path(), &checkout, &commit, case);
        let expected_head =
            matches!(case, "head" | "conflict").then(|| git(&checkout, &["rev-parse", "HEAD"]));
        let git_dirs = [checkout.join(".git"), checkout.join(".git/modules/nested")];
        let snapshot = || {
            git_dirs
                .iter()
                .map(|git_dir| {
                    (
                        fs::read(git_dir.join("index")).ok(),
                        fs::metadata(git_dir.join("index"))
                            .and_then(|m| m.modified())
                            .ok(),
                        fs::metadata(git_dir).and_then(|m| m.modified()).ok(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let before = snapshot();
        let report = inspect_checkouts(&home).unwrap();
        let read_only = before == snapshot()
            && git_dirs
                .iter()
                .all(|git_dir| !git_dir.join("index.lock").exists());
        let expected_paths = match case {
            "modified" | "deleted" | "conflict" => vec!["tracked.txt"],
            "untracked" => vec!["untracked dir/new\nfile.txt"],
            "renamed" => vec!["renamed.txt", "tracked.txt"],
            _ => Vec::new(),
        };
        let valid = report.checkouts.len() == 1
            && report.checkouts.first().is_some_and(|row| {
                row.packs == [name("crew")]
                    && row.commit == commit
                    && row.path == checkout
                    && (case != "read-only" || row.reinstall.ends_with("@main"))
                    && match &row.state {
                        CheckoutState::Clean => matches!(case, "clean" | "ignored" | "read-only"),
                        CheckoutState::Modified { head, changed } => {
                            matches!(
                                case,
                                "modified"
                                    | "deleted"
                                    | "untracked"
                                    | "head"
                                    | "renamed"
                                    | "conflict"
                            ) && *head == expected_head
                                && changed == &expected_paths
                        }
                        CheckoutState::Unreadable { error } => {
                            case == "missing-git" && error.contains(".git")
                        }
                    }
            });
        results.push((case, valid && read_only, report));
    }
    assert_eq!(
        results
            .iter()
            .map(|(_, valid, _)| *valid)
            .collect::<Vec<_>>(),
        vec![true; cases.len()],
        "{results:#?}"
    );
}
