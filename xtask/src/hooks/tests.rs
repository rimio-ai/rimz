use std::fs;
use std::path::Path;
use std::process::Command;

use super::fmt_staged;

const UNFORMATTED: &str = "fn example( ){let value=1;}\n";
const FORMATTED: &str = "fn example() {\n    let value = 1;\n}\n";

fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "--quiet"]);
    for (key, value) in [
        ("user.name", "Hook Test"),
        ("user.email", "hook@example.invalid"),
        ("core.hooksPath", "/dev/null"),
        ("commit.gpgSign", "false"),
    ] {
        git(root.path(), &["config", key, value]);
    }
    fs::write(
        root.path().join("rustfmt.toml"),
        include_str!("../../../rustfmt.toml"),
    )
    .unwrap();
    for (path, source) in files {
        fs::write(root.path().join(path), source).unwrap();
    }
    git(root.path(), &["add", "."]);
    git(
        root.path(),
        &["commit", "--quiet", "--no-verify", "-m", "base"],
    );
    root
}

#[test]
fn docs_commit_ignores_unstaged_untracked_and_committed_rust() {
    let root = repo(&[("unstaged.rs", FORMATTED), ("committed.rs", UNFORMATTED)]);
    fs::write(root.path().join("unstaged.rs"), UNFORMATTED).unwrap();
    fs::write(root.path().join("untracked.rs"), UNFORMATTED).unwrap();
    fs::write(root.path().join("note.md"), "a staged doc\n").unwrap();
    git(root.path(), &["add", "note.md"]);

    let result = fmt_staged(root.path());

    assert!(
        result.is_ok(),
        "uncommitted Rust must not refuse docs: {result:?}"
    );
    for path in ["unstaged.rs", "untracked.rs", "committed.rs"] {
        assert_eq!(
            fs::read(root.path().join(path)).unwrap(),
            UNFORMATTED.as_bytes()
        );
    }
}

#[test]
fn staged_parent_does_not_follow_unstaged_child_modules() {
    let root = repo(&[("lib.rs", "mod child;\n"), ("child.rs", FORMATTED)]);
    fs::write(root.path().join("lib.rs"), "pub mod child;\n").unwrap();
    fs::write(root.path().join("child.rs"), UNFORMATTED).unwrap();
    git(root.path(), &["add", "lib.rs"]);

    let result = fmt_staged(root.path());

    assert!(
        result.is_ok(),
        "unstaged children must not be checked: {result:?}"
    );
    assert_eq!(
        fs::read(root.path().join("child.rs")).unwrap(),
        UNFORMATTED.as_bytes()
    );
}

#[test]
fn refused_index_formats_only_its_working_copy_without_staging() {
    let root = repo(&[("lib.rs", ""), ("child.rs", FORMATTED)]);
    let path = root.path().join("lib.rs");
    fs::write(&path, format!("mod child;\n{UNFORMATTED}")).unwrap();
    fs::write(root.path().join("child.rs"), UNFORMATTED).unwrap();
    git(root.path(), &["add", "lib.rs"]);
    let index_before = git(root.path(), &["rev-parse", ":lib.rs"]);

    let err = fmt_staged(root.path()).unwrap_err().to_string();

    assert_eq!(
        fs::read(&path).unwrap(),
        format!("mod child;\n{FORMATTED}").as_bytes()
    );
    assert_eq!(git(root.path(), &["rev-parse", ":lib.rs"]), index_before);
    assert_eq!(
        fs::read(root.path().join("child.rs")).unwrap(),
        UNFORMATTED.as_bytes()
    );
    assert_eq!(err.matches("lib.rs").count(), 1, "{err}");
    assert!(err.contains("working copy formatted"), "{err}");
    assert!(err.contains("re-stage"), "{err}");
}

#[test]
fn refused_index_with_formatted_working_copy_requires_restaging_without_a_write() {
    let root = repo(&[("example.rs", FORMATTED)]);
    let path = root.path().join("example.rs");
    fs::write(&path, UNFORMATTED).unwrap();
    git(root.path(), &["add", "example.rs"]);
    let index_before = git(root.path(), &["rev-parse", ":example.rs"]);
    fs::write(&path, FORMATTED).unwrap();
    let modified_before = fs::metadata(&path).unwrap().modified().unwrap();

    let err = fmt_staged(root.path()).unwrap_err().to_string();

    assert!(err.contains("working copy already formatted"), "{err}");
    assert!(err.contains("re-stage"), "{err}");
    assert_eq!(fs::read(&path).unwrap(), FORMATTED.as_bytes());
    assert_eq!(
        fs::metadata(&path).unwrap().modified().unwrap(),
        modified_before
    );
    assert_eq!(
        git(root.path(), &["rev-parse", ":example.rs"]),
        index_before
    );
}

#[test]
fn formatted_index_leaves_unformatted_unstaged_hunks_untouched() {
    let root = repo(&[("example.rs", "")]);
    let path = root.path().join("example.rs");
    fs::write(&path, FORMATTED).unwrap();
    git(root.path(), &["add", "example.rs"]);
    fs::write(&path, UNFORMATTED).unwrap();

    let result = fmt_staged(root.path());

    assert!(result.is_ok(), "only the index should decide: {result:?}");
    assert_eq!(fs::read(&path).unwrap(), UNFORMATTED.as_bytes());
}

#[test]
fn renamed_unformatted_index_is_refused_at_the_destination_path() {
    let root = repo(&[("old.rs", UNFORMATTED)]);
    git(root.path(), &["mv", "old.rs", "new.rs"]);
    assert!(git(root.path(), &["diff", "--cached", "--name-status"]).starts_with(b"R100\t"));

    let err = fmt_staged(root.path()).unwrap_err().to_string();

    assert_eq!(
        fs::read(root.path().join("new.rs")).unwrap(),
        FORMATTED.as_bytes()
    );
    assert_eq!(err.matches("new.rs").count(), 1, "{err}");
    assert!(!err.contains("old.rs"), "{err}");
    assert!(!root.path().join("old.rs").exists());
}

#[test]
fn deleted_and_intent_to_add_files_are_not_judged() {
    let root = repo(&[("deleted.rs", UNFORMATTED)]);
    git(root.path(), &["rm", "deleted.rs"]);
    fs::write(root.path().join("intent.rs"), UNFORMATTED).unwrap();
    git(root.path(), &["add", "-N", "intent.rs"]);

    let result = fmt_staged(root.path());

    assert!(
        result.is_ok(),
        "deleted and intent-to-add must be ignored: {result:?}"
    );
    assert!(!root.path().join("deleted.rs").exists());
    assert_eq!(
        fs::read(root.path().join("intent.rs")).unwrap(),
        UNFORMATTED.as_bytes()
    );
}

#[test]
fn index_parse_error_is_reported_and_a_valid_working_copy_is_formatted() {
    let root = repo(&[("example.rs", "")]);
    fs::write(root.path().join("example.rs"), "fn broken(\n").unwrap();
    git(root.path(), &["add", "example.rs"]);
    let index_before = git(root.path(), &["rev-parse", ":example.rs"]);
    fs::write(root.path().join("example.rs"), UNFORMATTED).unwrap();

    let err = fmt_staged(root.path()).unwrap_err().to_string();

    assert!(err.contains("unclosed delimiter"), "{err}");
    assert!(err.contains("working copy formatted"), "{err}");
    assert_eq!(
        fs::read(root.path().join("example.rs")).unwrap(),
        FORMATTED.as_bytes()
    );
    assert_eq!(
        git(root.path(), &["rev-parse", ":example.rs"]),
        index_before
    );
}

#[test]
fn refusal_reports_all_paths_and_leaves_missing_or_invalid_working_copies_alone() {
    let root = repo(&[("missing.rs", ""), ("invalid.rs", "")]);
    for path in ["missing.rs", "invalid.rs"] {
        fs::write(root.path().join(path), UNFORMATTED).unwrap();
    }
    git(root.path(), &["add", "."]);
    fs::remove_file(root.path().join("missing.rs")).unwrap();
    fs::write(root.path().join("invalid.rs"), "fn broken(\n").unwrap();
    let index_before = git(root.path(), &["diff", "--cached"]);

    let err = fmt_staged(root.path()).unwrap_err().to_string();

    for path in ["missing.rs", "invalid.rs"] {
        assert_eq!(err.matches(path).count(), 1, "{err}");
    }
    assert!(err.contains("No such file"), "{err}");
    assert!(err.contains("unclosed delimiter"), "{err}");
    assert!(!root.path().join("missing.rs").exists());
    assert_eq!(
        fs::read(root.path().join("invalid.rs")).unwrap(),
        b"fn broken(\n"
    );
    assert_eq!(git(root.path(), &["diff", "--cached"]), index_before);
}

#[test]
fn staged_paths_are_nul_separated_and_use_the_workspace_rustfmt_config() {
    let root = repo(&[]);
    let path = "space and\nnewline.rs";
    fs::write(
        root.path().join(path),
        "fn example(value: u8) -> S { S { value: value } }\n",
    )
    .unwrap();
    git(root.path(), &["add", "--", path]);

    let err = fmt_staged(root.path()).unwrap_err().to_string();

    assert_eq!(
        fs::read(root.path().join(path)).unwrap(),
        b"fn example(value: u8) -> S {\n    S { value }\n}\n"
    );
    assert!(err.contains("working copy formatted"), "{err}");
}
