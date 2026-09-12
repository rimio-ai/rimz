//! Room output layout and streaming file measurements.

use rimz::disk::paths::StatePaths;
use rimz::disk::summary::FileSummary;

#[test]
fn room_tmp_layout_is_private_and_created_on_demand() {
    let root = tempfile::tempdir().unwrap();
    let paths = StatePaths::under(
        rimz::WorkspaceId::from_project_root(root.path()),
        root.path(),
    )
    .unwrap();
    paths.ensure_dirs().unwrap();
    assert!(!paths.tmp_dir.exists());
    paths.ensure_tmp_dir().unwrap();
    for directory in [
        &paths.scratchpad_dir,
        &paths.waits_dir,
        &paths.subagents_dir,
    ] {
        assert!(directory.is_dir());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&paths.tmp_dir)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    paths.remove_tmp_dir().unwrap();
    assert!(!paths.tmp_dir.exists());
}

#[test]
fn output_summary_counts_physical_lines_and_all_bytes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("response.output");
    for (bytes, lines) in [
        (b"".as_slice(), 0),
        (b"a\nb\n", 2),
        (b"a\nb", 2),
        (b"\n\n", 2),
        (b"\xff\n", 1),
    ] {
        std::fs::write(&path, bytes).unwrap();
        let summary = FileSummary::measure(&path).unwrap();
        assert_eq!(
            summary,
            FileSummary {
                bytes: bytes.len() as u64,
                lines
            }
        );
        assert_eq!(
            summary.lines_label(),
            if lines == 1 {
                "1 line".to_owned()
            } else {
                format!("{lines} lines")
            }
        );
    }
    let bytes = "x\n".repeat(20_000) + "last";
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(
        FileSummary::measure(&path).unwrap(),
        FileSummary {
            bytes: bytes.len() as u64,
            lines: 20_001
        }
    );
}
