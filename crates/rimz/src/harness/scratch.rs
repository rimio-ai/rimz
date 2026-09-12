//! Shared team memory-file discovery and advisory blackboard stage parsing.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

const MATCH_OPTIONS: glob::MatchOptions = glob::MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScratchFile {
    pub path: PathBuf,
    pub lines: usize,
    pub modified: Option<SystemTime>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScratchScan {
    pub files: Vec<ScratchFile>,
    pub probe_failed: bool,
}

/// Scan root-anchored patterns, returning absolute paths in sorted, deduplicated order.
pub fn scan(root: &Path, patterns: &[String]) -> ScratchScan {
    let mut scan = ScratchScan::default();
    let root = match std::path::absolute(root) {
        Ok(root) => root,
        Err(err) => {
            tracing::warn!(error = %err, "could not resolve team memory root");
            scan.probe_failed = true;
            return scan;
        }
    };
    for pattern in patterns {
        let pattern = pattern.strip_prefix('/').unwrap_or(pattern);
        let rooted = format!(
            "{}/{}",
            glob::Pattern::escape(&root.to_string_lossy()),
            pattern
        );
        let matches = match glob::glob_with(&rooted, MATCH_OPTIONS) {
            Ok(matches) => matches,
            Err(err) => {
                tracing::warn!(pattern, error = %err, "could not probe team memory pattern");
                scan.probe_failed = true;
                continue;
            }
        };
        for entry in matches {
            let path = match entry {
                Ok(path) => path,
                Err(err) => {
                    tracing::warn!(pattern, error = %err, "could not read team memory match");
                    scan.probe_failed = true;
                    continue;
                }
            };
            if !path.is_file() || !path.starts_with(&root) {
                continue;
            }
            let lines = std::fs::read_to_string(&path)
                .map(|text| text.lines().count())
                .unwrap_or(0);
            let modified = std::fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .ok();
            scan.files.push(ScratchFile {
                path,
                lines,
                modified,
            });
        }
    }
    scan.files.sort_by(|left, right| left.path.cmp(&right.path));
    scan.files.dedup_by(|left, right| left.path == right.path);
    scan
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardStage {
    pub name: String,
    pub owner: Option<String>,
}

/// Read the first `Stage:` line, separating only a terminal ` (@owner)` suffix.
pub fn board_stage(root: &Path) -> Option<BoardStage> {
    let board = std::fs::read_to_string(root.join("blackboard.md")).ok()?;
    parse_board_stage(&board)
}

pub(super) fn parse_board_stage(board: &str) -> Option<BoardStage> {
    let stage = board
        .lines()
        .find_map(|line| line.strip_prefix("Stage:"))?
        .trim();
    let (name, owner) = stage
        .strip_suffix(')')
        .and_then(|stage| stage.rsplit_once(" (@"))
        .filter(|(_, owner)| {
            !owner.is_empty() && !owner.contains(['(', ')']) && !owner.contains(char::is_whitespace)
        })
        .map_or((stage, None), |(name, owner)| {
            (name, Some(owner.to_owned()))
        });
    Some(BoardStage {
        name: name.to_owned(),
        owner,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_preserves_rooted_matching_deduplication_and_probe_failures() {
        let worktree = tempfile::tempdir().expect("worktree");
        let root = worktree.path().join("root[1]");
        std::fs::create_dir(&root).expect("root with glob syntax");
        std::fs::write(root.join("blackboard.md"), "one\ntwo\n").expect("board");
        std::fs::write(root.join("plan-notes.md"), "one\n").expect("plan");
        std::fs::write(root.join("review-notes.md"), [0xff]).expect("unreadable text");
        std::fs::write(root.join(".hidden-notes.md"), "hidden").expect("hidden notes");
        std::fs::create_dir(root.join("state")).expect("state directory");
        std::fs::write(root.join("state/nested-notes.md"), "nested").expect("nested notes");
        let patterns = [
            "/blackboard.md",
            "missing.md",
            "/*-notes.md",
            "state/",
            "plan-notes.md",
        ]
        .map(str::to_owned);
        let result = scan(&root, &patterns);
        assert!(!result.probe_failed);
        assert_eq!(
            result
                .files
                .iter()
                .map(|file| (file.path.clone(), file.lines))
                .collect::<Vec<_>>(),
            [
                (root.join(".hidden-notes.md"), 1),
                (root.join("blackboard.md"), 2),
                (root.join("plan-notes.md"), 1),
                (root.join("review-notes.md"), 0),
            ]
        );
        assert!(
            result
                .files
                .iter()
                .all(|file| file.path.is_absolute() && file.modified.is_some())
        );
        let mut invalid_patterns = patterns.to_vec();
        invalid_patterns.push("[abc.md".to_owned());
        let failed = scan(&root, &invalid_patterns);
        assert!(failed.probe_failed);
        assert_eq!(failed.files, result.files);
        assert!(scan(&root, &[]).files.is_empty());
    }

    #[test]
    fn board_stage_keeps_compound_names_and_optional_owner() {
        let worktree = tempfile::tempdir().expect("worktree");
        let board = worktree.path().join("blackboard.md");
        assert_eq!(board_stage(worktree.path()), None);
        for (text, name, owner) in [
            (
                "# Board\nStage: Implement (delta) (@coder)\nStage: Review (@reviewer)",
                "Implement (delta)",
                Some("coder"),
            ),
            ("Stage: Implement (delta)", "Implement (delta)", None),
            ("Stage: Plan (@planner) more", "Plan (@planner) more", None),
            ("Stage: Plan (@planner)\r\n", "Plan", Some("planner")),
            (
                " Stage: ignored\nStage: Review  changes",
                "Review  changes",
                None,
            ),
        ] {
            std::fs::write(&board, text).expect("board");
            assert_eq!(
                board_stage(worktree.path()),
                Some(BoardStage {
                    name: name.to_owned(),
                    owner: owner.map(str::to_owned),
                })
            );
        }
        std::fs::write(&board, "# No stage").expect("board without stage");
        assert_eq!(board_stage(worktree.path()), None);
        std::fs::write(&board, [0xff]).expect("unreadable text");
        assert_eq!(board_stage(worktree.path()), None);
    }
}
