//! Shared team memory-file discovery and advisory blackboard stage parsing.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use jiff::{Timestamp, civil::DateTime, tz::TimeZone};

use crate::config::DONE_STAGE;

pub(super) const PROGRESS_STAMP_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

fn parse_progress_stamp(stamp: &str, zone: &TimeZone) -> Option<Timestamp> {
    DateTime::strptime(PROGRESS_STAMP_FORMAT, stamp)
        .or_else(|_| DateTime::strptime("%Y-%m-%d %H:%M", stamp))
        .ok()?
        .to_zoned(zone.clone())
        .ok()
        .map(|at| at.timestamp())
}

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardRun {
    pub stage: BoardStage,
    pub started_at: Option<Timestamp>,
    pub done_at: Option<Timestamp>,
}

/// Read the current stage and run times from one blackboard snapshot.
pub fn board_run(root: &Path, zone: &TimeZone) -> Option<BoardRun> {
    let board = std::fs::read_to_string(root.join("blackboard.md")).ok()?;
    parse_board_run(&board, zone)
}

fn parse_board_run(board: &str, zone: &TimeZone) -> Option<BoardRun> {
    let mut run = BoardRun {
        stage: parse_board_stage(board)?,
        started_at: None,
        done_at: None,
    };
    let progress = parse_board_sections(board, &["Progress", "Progress log"]);
    for line in progress.as_deref().unwrap_or_default().lines() {
        let Some(entry) = line.strip_prefix("- ") else {
            continue;
        };
        let entry = entry
            .split_once(" — ")
            .map_or(entry.trim_end(), |(entry, _)| entry);
        let Some((stamp, entry)) = entry.split_once(" @") else {
            continue;
        };
        let Some((by, transition)) = entry.split_once(": ") else {
            continue;
        };
        if by.is_empty() || by.contains(char::is_whitespace) {
            continue;
        }
        let (from, to) = match transition.split_once(" -> ") {
            Some((from, to)) if !from.is_empty() => (Some(from), to),
            _ => match transition.strip_prefix("opened ") {
                Some(to) => (None, to),
                None => continue,
            },
        };
        if to.is_empty() {
            continue;
        }
        let Some(at) = parse_progress_stamp(stamp, zone) else {
            continue;
        };
        if run.started_at.is_none() || from == Some(DONE_STAGE) {
            run.started_at = Some(at);
        }
        if to == DONE_STAGE && run.stage.name == DONE_STAGE {
            run.done_at = Some(at);
        }
    }
    Some(run)
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

/// Read the trimmed text under `## <heading>`, up to the next ATX heading
/// outside a fenced code block; `None` when the heading is absent or the
/// section is blank.
pub fn board_section(root: &Path, heading: &str) -> Option<String> {
    let board = std::fs::read_to_string(root.join("blackboard.md")).ok()?;
    parse_board_section(&board, heading)
}

fn parse_board_section(board: &str, heading: &str) -> Option<String> {
    parse_board_sections(board, &[heading])
}

fn parse_board_sections(board: &str, headings: &[&str]) -> Option<String> {
    let section = board
        .lines()
        .skip_while(|line| {
            !line
                .strip_prefix("## ")
                .is_some_and(|heading| headings.contains(&heading.trim()))
        })
        .skip(1)
        .scan(false, |in_fence, line| {
            if ["```", "~~~"].iter().any(|fence| line.starts_with(fence)) {
                *in_fence = !*in_fence;
            }
            (*in_fence || !is_atx_heading(line)).then_some(line)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let section = section.trim();
    (!section.is_empty()).then(|| section.to_owned())
}

fn is_atx_heading(line: &str) -> bool {
    let text = line.trim_start_matches('#');
    (1..=6).contains(&(line.len() - text.len()))
        && (text.is_empty() || text.starts_with([' ', '\t']))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_run_reads_stage_and_optional_ledger() {
        let worktree = tempfile::tempdir().unwrap();
        assert_eq!(board_run(worktree.path(), &TimeZone::UTC), None);
        for board in [
            "# Board",
            "## Progress\n- 2026-09-12 14:02 @user: opened Plan — start",
        ] {
            assert_eq!(parse_board_run(board, &TimeZone::UTC), None);
        }
        std::fs::write(
            worktree.path().join("blackboard.md"),
            "Stage: Plan (@planner)",
        )
        .unwrap();
        let run = board_run(worktree.path(), &TimeZone::UTC).unwrap();
        assert_eq!(run.stage.name, "Plan");
        assert_eq!(run.stage.owner.as_deref(), Some("planner"));
        assert_eq!((run.started_at, run.done_at), (None, None));
    }

    #[test]
    fn board_run_tracks_first_start_restarts_and_last_done() {
        let zone = TimeZone::get("Asia/Kolkata").unwrap();
        let ledger = "- garbage\n- 2026-09-12 14:00 @user: nonsense — ignored\n- invalid @user: opened Plan — ignored\n- 2026-09-12 14:02 @user: opened Plan — start\n- 2026-09-12 14:03:07 @planner: Plan -> Plan — reopen\n- 2026-09-12 14:04:08 @planner: Plan -> Done — finish";
        let first = "2026-09-12T08:32:00Z".parse().unwrap();
        let finish = "2026-09-12T08:34:08Z".parse().unwrap();
        for heading in ["Progress", "Progress log"] {
            for stage in ["Plan", "Done"] {
                let board = format!(
                    "Stage: {stage}\n## {heading}\n{ledger}\n### Evidence\n- 2026-09-12 14:05 @user: Done -> Plan — outside ledger"
                );
                let run = parse_board_run(&board, &zone).unwrap();
                assert_eq!(run.started_at, Some(first));
                assert_eq!(run.done_at, (stage == "Done").then_some(finish));
            }
        }
        let board = format!(
            "Stage: Done\n## Progress\n{ledger}\n- 2026-09-12 14:06:09 @user: Done -> Plan — restart\n- 2026-09-12 14:07:10 @planner: Plan -> Done — finish again"
        );
        let run = parse_board_run(&board, &zone).unwrap();
        assert_eq!(
            run.started_at,
            Some("2026-09-12T08:36:09Z".parse().unwrap())
        );
        assert_eq!(run.done_at, Some("2026-09-12T08:37:10Z".parse().unwrap()));
    }

    #[test]
    fn board_run_does_not_parse_transitions_in_notes() {
        let board = "Stage: Done\n## Progress\n- 2026-09-12 14:02:03 @user: Plan -> Plan — later -> Done\n- 2026-09-12 14:04:05 @user: opened Plan — Done -> Plan";
        let run = parse_board_run(board, &TimeZone::UTC).unwrap();
        assert_eq!(
            run.started_at,
            Some("2026-09-12T14:02:03Z".parse().unwrap())
        );
        assert_eq!(run.done_at, None);
    }

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

    #[test]
    fn board_section_reads_to_the_next_heading_and_drops_blank_sections() {
        let worktree = tempfile::tempdir().expect("worktree");
        assert_eq!(board_section(worktree.path(), "Result"), None);
        let text = "# Blackboard\r\nStage: Done\r\n\r\n## Results\r\nwrong\r\n## Result\r\n\r\nPR: https://x/1\r\n#412 merged\r\n```sh\r\n# rerun the gate\r\n## not a heading\r\n```\r\n- tests pass\r\n\r\n### Detail\r\nnot included\r\n## Empty\r\n  \r\n## Tail\r\nlast";
        for (heading, section) in [
            (
                "Result",
                Some(
                    "PR: https://x/1\n#412 merged\n```sh\n# rerun the gate\n## not a heading\n```\n- tests pass",
                ),
            ),
            ("Results", Some("wrong")),
            ("Empty", None),
            ("Tail", Some("last")),
            ("Missing", None),
            ("Detail", None),
        ] {
            assert_eq!(
                parse_board_section(text, heading).as_deref(),
                section,
                "{heading}"
            );
        }
        std::fs::write(worktree.path().join("blackboard.md"), text).expect("board");
        assert_eq!(
            board_section(worktree.path(), "Tail").as_deref(),
            Some("last")
        );
    }
}
