use super::*;

#[test]
fn strip_deleted_suffix_removes_only_the_kernel_annotation() {
    assert_eq!(
        strip_deleted_suffix(Path::new("/usr/bin/rimz-sidebar (deleted)")),
        Some(PathBuf::from("/usr/bin/rimz-sidebar"))
    );
    // A path the kernel did not annotate is left alone.
    assert_eq!(
        strip_deleted_suffix(Path::new("/usr/bin/rimz-sidebar")),
        None
    );
    // " (deleted)" only counts as a trailing suffix, never mid-path.
    assert_eq!(
        strip_deleted_suffix(Path::new("/opt/my (deleted)/rimz-sidebar")),
        None
    );
}

#[test]
fn reexec_target_resolves_the_replacement_after_an_install() {
    // Post-`cargo install`: the inode behind our `current_exe()` was
    // unlinked, so it reads "<path> (deleted)" while the freshly-installed
    // binary now sits at the un-annotated path — that is what we re-exec.
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("rimz-sidebar");
    std::fs::write(&real, b"x").unwrap();
    let deleted = PathBuf::from(format!("{} (deleted)", real.display()));
    assert!(!deleted.is_file(), "the annotated path must not exist");
    assert_eq!(resolve_reexec_target(deleted), Some(real.clone()));
    // The ordinary, not-replaced case uses the live path as-is.
    assert_eq!(resolve_reexec_target(real.clone()), Some(real));
}

#[test]
fn reexec_target_is_none_when_nothing_exists_on_disk() {
    // A partial or in-flight install: neither the annotated nor the
    // stripped path is a file, so the loop keeps serving the current build
    // rather than re-execing into nothing and vanishing.
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("rimz-sidebar");
    let deleted = PathBuf::from(format!("{} (deleted)", missing.display()));
    assert_eq!(resolve_reexec_target(deleted), None);
    assert_eq!(resolve_reexec_target(missing), None);
}

#[test]
fn decide_reload_reexecs_only_when_the_on_disk_binary_differs() {
    let target = PathBuf::from("/some/rimz-sidebar");
    // Byte-identical to what we run: skip the re-exec churn.
    assert!(matches!(
        decide_reload(Some(target.clone()), Some(true)),
        ReloadAction::AlreadyCurrent
    ));
    // Content differs: re-exec onto the freshly-installed build.
    assert!(matches!(
        decide_reload(Some(target.clone()), Some(false)),
        ReloadAction::Reexec(t) if t == target
    ));
    // Running image unreadable (non-Linux / IO race): re-exec, preserving
    // the always-load-the-on-disk-build behavior.
    assert!(matches!(
        decide_reload(Some(target.clone()), None),
        ReloadAction::Reexec(t) if t == target
    ));
    // No binary on disk: keep the current build regardless of the compare.
    assert!(matches!(decide_reload(None, None), ReloadAction::Missing));
    assert!(matches!(
        decide_reload(None, Some(true)),
        ReloadAction::Missing
    ));
}

#[test]
fn same_file_contents_detects_byte_equality() {
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("original");
    let identical = dir.path().join("identical");
    let same_len_differs = dir.path().join("same_len_differs");
    let shorter = dir.path().join("shorter");
    std::fs::write(&original, b"freshly-installed build").unwrap();
    std::fs::write(&identical, b"freshly-installed build").unwrap();
    std::fs::write(&same_len_differs, b"freshly-installed BUILD").unwrap();
    std::fs::write(&shorter, b"shorter").unwrap();
    assert!(same_file_contents(&original, &identical).unwrap());
    assert!(!same_file_contents(&original, &same_len_differs).unwrap());
    assert!(!same_file_contents(&original, &shorter).unwrap());
}
