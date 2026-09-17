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
        &paths.agents_dir,
        &paths.shared_dir,
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
fn output_summary_counts_physical_lines_all_bytes_and_tokens() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("response.output");
    for (bytes, lines, tokens, label) in [
        (b"".as_slice(), 0, 0, "<1k tokens, 0 lines"),
        (b"a\nb\n", 2, 4, "<1k tokens, 2 lines"),
        (b"a\nb", 2, 3, "<1k tokens, 2 lines"),
        (b"\n\n", 2, 1, "<1k tokens, 2 lines"),
        (b"\xff\n", 1, 1, "<1k tokens, 1 line"),
    ] {
        std::fs::write(&path, bytes).unwrap();
        let summary = FileSummary::measure(&path).unwrap();
        assert_eq!(
            summary,
            FileSummary {
                bytes: bytes.len() as u64,
                lines,
                tokens,
            }
        );
        assert_eq!(summary.is_empty(), bytes.is_empty());
        assert_eq!(summary.label(), label);
    }
    let bytes = "x\n".repeat(20_000) + "last";
    std::fs::write(&path, &bytes).unwrap();
    let summary = FileSummary::measure(&path).unwrap();
    assert_eq!((summary.bytes, summary.lines), (bytes.len() as u64, 20_001));
    assert_eq!(summary.tokens, rimz::utils::tokens::estimate(&bytes));
}

#[test]
fn output_summary_scales_tokens_past_the_sample() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("wait.output");
    let text = (0..60_000)
        .map(|line| format!("test harness::case_{line} ... ok ({} ms)\n", line % 97))
        .collect::<String>();
    assert!(text.len() > 2 << 20);
    std::fs::write(&path, &text).unwrap();
    let summary = FileSummary::measure(&path).unwrap();
    let exact = rimz::utils::tokens::estimate(&text);
    assert!(
        summary.tokens.abs_diff(exact) * 100 <= exact * 3,
        "scaled {} vs exact {exact}",
        summary.tokens
    );
}
