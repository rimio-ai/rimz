use super::*;

#[test]
fn decide_reload_reexecs_only_when_the_on_disk_binary_differs() {
    let target = PathBuf::from("/some/rimz");
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
