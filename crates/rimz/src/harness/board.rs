//! Locked write-side mechanics for the team blackboard.

use std::path::{Path, PathBuf};

use jiff::Timestamp;

use crate::Store;
use crate::config::MachineConfig;
use crate::disk::{atomic, lock::WorkspaceLock};

use super::scratch::is_atx_heading;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoardSection {
    Goal,
    Decisions,
    Result,
}

impl BoardSection {
    pub fn heading(self) -> &'static str {
        match self {
            Self::Goal => "## Goal",
            Self::Decisions => "## Decisions",
            Self::Result => "## Result",
        }
    }
}

impl std::str::FromStr for BoardSection {
    type Err = BoardErr;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "goal" => Ok(Self::Goal),
            "decisions" => Ok(Self::Decisions),
            "result" => Ok(Self::Result),
            "progress" | "progress log" | "stage" => Err(BoardErr::FlipSection(value.to_owned())),
            _ => Err(BoardErr::UnknownSection(value.to_owned())),
        }
    }
}

pub struct RecordRequest<'a> {
    pub store: &'a Store,
    pub worktree: &'a Path,
    pub section: BoardSection,
    pub by: &'a str,
    pub text: &'a str,
    pub now: Timestamp,
}

#[derive(Debug)]
pub struct RecordReceipt {
    pub board: PathBuf,
    pub entry: String,
}

pub fn record(request: RecordRequest<'_>) -> Result<RecordReceipt, BoardErr> {
    let entry = entry_line(request.now, request.by, request.text)?;
    let board = LockedBoard::open(request.store, request.worktree)?;
    let text = if board.text.is_empty() {
        "# Blackboard\n\n## Goal\n\n## Decisions\n\n## Progress\n\n## Result\n"
    } else {
        &board.text
    };
    board.write(&append_section(text, request.section.heading(), &entry))?;
    Ok(RecordReceipt {
        board: board.path,
        entry,
    })
}

fn append_section(text: &str, heading: &str, entry: &str) -> String {
    const ORDER: [&str; 4] = ["Goal", "Decisions", "Progress", "Result"];
    let rank = |title: &str| {
        let title = if title == "Progress log" {
            "Progress"
        } else {
            title
        };
        ORDER.iter().position(|candidate| *candidate == title)
    };
    let name = heading.trim_start_matches("## ");
    let mut offset = 0;
    let mut insertion = None;
    let mut later = None;
    let mut in_fence = false;
    for line in text.split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        let boundary = !in_fence && is_atx_heading(content);
        if content.starts_with("```") || content.starts_with("~~~") {
            in_fence = !in_fence;
        }
        if insertion.is_some() && boundary {
            break;
        }
        let title = boundary
            .then(|| content.strip_prefix("## "))
            .flatten()
            .map(str::trim);
        if later.is_none()
            && title
                .and_then(rank)
                .is_some_and(|order| Some(order) > rank(name))
        {
            later = Some(offset);
        }
        offset += line.len();
        if title
            .is_some_and(|title| title == name || (name == "Progress" && title == "Progress log"))
            || (insertion.is_some() && !content.trim().is_empty())
        {
            insertion = Some(offset);
        }
    }
    let mut result = text.to_owned();
    if let Some(at) = insertion {
        let prefix = if text[..at].ends_with('\n') { "" } else { "\n" };
        result.insert_str(at, &format!("{prefix}{entry}\n"));
        return result;
    }
    let at = later.unwrap_or(text.len());
    let prefix = if at == 0 || text[..at].lines().next_back() == Some("") {
        ""
    } else if text[..at].ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    let suffix = if at < text.len() { "\n" } else { "" };
    result.insert_str(at, &format!("{prefix}{heading}\n{entry}\n{suffix}"));
    result
}

fn entry_line(now: Timestamp, by: &str, text: &str) -> Result<String, BoardErr> {
    let text = text.replace('\r', "");
    let text = text.trim();
    if text.is_empty() {
        return Err(BoardErr::BlankText);
    }
    let mut entry = format!("- {} @{by}: ", stamp(now));
    for (index, line) in text.split('\n').enumerate() {
        if index != 0 {
            entry.push('\n');
            if !line.is_empty() {
                entry.push_str("  ");
            }
        }
        entry.push_str(line);
    }
    Ok(entry)
}

pub(super) fn stamp(now: Timestamp) -> String {
    now.to_zoned(MachineConfig::load_lenient().time_zone())
        .strftime(super::scratch::PROGRESS_STAMP_FORMAT)
        .to_string()
}

#[derive(Debug, thiserror::Error)]
pub enum BoardErr {
    #[error("`{0}` belongs to `rimz teams flip`; record takes Goal, Decisions, or Result")]
    FlipSection(String),
    #[error("provide nonblank text for the board entry")]
    BlankText,
    #[error("unknown board section `{0}`; choose Goal, Decisions, or Result")]
    UnknownSection(String),
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Lock(#[from] crate::disk::lock::LockErr),
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
}

pub(super) struct LockedBoard {
    pub worktree: PathBuf,
    pub path: PathBuf,
    pub text: String,
    _lock: WorkspaceLock,
}

impl LockedBoard {
    pub(super) fn open(store: &Store, worktree: &Path) -> Result<Self, BoardErr> {
        let worktree = canonical_worktree(worktree)?;
        let path = worktree.join("blackboard.md");
        let lock = WorkspaceLock::acquire(&store.runtime_paths().board_lock(&worktree))?;
        let text = read_board(&path)?;
        Ok(Self {
            worktree,
            path,
            text,
            _lock: lock,
        })
    }

    pub(super) fn write(&self, text: &str) -> Result<(), BoardErr> {
        atomic::write_bytes_atomically(&self.path, text.as_bytes())?;
        Ok(())
    }
}

fn canonical_worktree(worktree: &Path) -> Result<PathBuf, BoardErr> {
    worktree.canonicalize().map_err(|source| BoardErr::Io {
        path: worktree.to_path_buf(),
        source,
    })
}

fn read_board(board: &Path) -> Result<String, BoardErr> {
    match std::fs::read_to_string(board) {
        Ok(text) => Ok(text),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(source) => Err(BoardErr::Io {
            path: board.to_path_buf(),
            source,
        }),
    }
}

pub(super) fn rewrite_board(text: &str, to: &str, owner: Option<&str>, ledger: &str) -> String {
    let mut result = String::with_capacity(text.len() + ledger.len() + to.len() + 64);
    let stage = match owner {
        Some(owner) => format!("Stage: {to} (@{owner})"),
        None => format!("Stage: {to}"),
    };
    let mut replaced = false;
    for line in text.split_inclusive('\n') {
        if !replaced && line.starts_with("Stage:") {
            result.push_str(&stage);
            if line.ends_with("\r\n") {
                result.push_str("\r\n");
            } else if line.ends_with('\n') {
                result.push('\n');
            }
            replaced = true;
        } else {
            result.push_str(line);
        }
    }
    if !replaced {
        let at = if text.starts_with("# ") {
            text.find('\n').map_or(text.len(), |at| at + 1)
        } else {
            0
        };
        let prefix = if at > 0 && !text[..at].ends_with('\n') {
            "\n"
        } else {
            ""
        };
        result.insert_str(at, &format!("{prefix}{stage}\n"));
    }
    append_section(&result, "## Progress", ledger)
}

#[cfg(test)]
mod tests {
    use super::super::team_stage::ledger_line;
    use super::*;

    #[test]
    fn record_creates_full_template_and_appends_to_empty_board() {
        let root = tempfile::tempdir().unwrap();
        let id = crate::WorkspaceId::from_project_root(root.path());
        let state = crate::StatePaths::under(id.clone(), &root.path().join("state")).unwrap();
        let runtime = crate::RuntimePaths::under(id, &root.path().join("runtime")).unwrap();
        let store = Store::open(state, runtime).unwrap();
        for empty in [false, true] {
            if empty {
                std::fs::write(root.path().join("blackboard.md"), "").unwrap();
            }
            let receipt = record(RecordRequest {
                store: &store,
                worktree: root.path(),
                section: BoardSection::Goal,
                by: "user",
                text: "Ship",
                now: Timestamp::UNIX_EPOCH,
            })
            .unwrap();
            assert_eq!(receipt.board, root.path().join("blackboard.md"));
            assert_eq!(
                std::fs::read_to_string(receipt.board).unwrap(),
                format!(
                    "# Blackboard\n\n## Goal\n{}\n\n## Decisions\n\n## Progress\n\n## Result\n",
                    receipt.entry
                )
            );
        }
    }

    #[test]
    fn section_names_accept_case_and_distinguish_flip_owned_names() {
        for (name, section) in [
            ("goal", BoardSection::Goal),
            ("DECISIONS", BoardSection::Decisions),
            ("Result", BoardSection::Result),
        ] {
            assert_eq!(name.parse::<BoardSection>().unwrap(), section);
            assert_eq!(
                section.heading(),
                format!(
                    "## {}",
                    match section {
                        BoardSection::Goal => "Goal",
                        BoardSection::Decisions => "Decisions",
                        BoardSection::Result => "Result",
                    }
                )
            );
        }
        for name in ["Progress", "Progress log", "Stage"] {
            assert_eq!(
                name.parse::<BoardSection>().unwrap_err().to_string(),
                format!(
                    "`{name}` belongs to `rimz teams flip`; record takes Goal, Decisions, or Result"
                )
            );
        }
        assert_eq!(
            "Evidence".parse::<BoardSection>().unwrap_err().to_string(),
            "unknown board section `Evidence`; choose Goal, Decisions, or Result"
        );
    }

    #[test]
    fn section_append_preserves_spacing_crlf_and_unterminated_body() {
        assert_eq!(
            append_section(
                "# Board\r\n\r\n## Goal\r\nFirst\r\n\r\nLast\r\n\r\n## Result\r\nKeep\r\n",
                "## Goal",
                "- next"
            ),
            "# Board\r\n\r\n## Goal\r\nFirst\r\n\r\nLast\r\n- next\n\r\n## Result\r\nKeep\r\n"
        );
        assert_eq!(
            append_section("## Goal\nlast", "## Goal", "- next"),
            "## Goal\nlast\n- next\n"
        );
    }

    #[test]
    fn section_bounds_match_the_board_reader() {
        for (board, heading, expected) in [
            (
                "## Result\nPR: x\n#412 merged\n- ok\n",
                "## Result",
                "## Result\nPR: x\n#412 merged\n- ok\n- NEW\n",
            ),
            (
                "## Goal\nRun:\n```sh\n# repro\ncargo test\n```\n\n## Result\n",
                "## Goal",
                "## Goal\nRun:\n```sh\n# repro\ncargo test\n```\n- NEW\n\n## Result\n",
            ),
            (
                "## Result \nPR: x\n",
                "## Result",
                "## Result \nPR: x\n- NEW\n",
            ),
            (
                "## Goal\nKeep\n\n## Result \nEnd\n",
                "## Decisions",
                "## Goal\nKeep\n\n## Decisions\n- NEW\n\n## Result \nEnd\n",
            ),
        ] {
            let appended = append_section(board, heading, "- NEW");
            assert_eq!(appended, expected);
            let root = tempfile::tempdir().unwrap();
            std::fs::write(root.path().join("blackboard.md"), &appended).unwrap();
            let section = heading.trim_start_matches("## ");
            assert!(
                super::super::scratch::board_section(root.path(), section)
                    .is_some_and(|text| text.ends_with("- NEW"))
            );
        }
    }

    #[test]
    fn missing_sections_follow_template_order() {
        assert_eq!(
            append_section(
                "## Goal\r\nKeep\r\n\r\n## Progress log\r\n",
                "## Decisions",
                "- next"
            ),
            "## Goal\r\nKeep\r\n\r\n## Decisions\n- next\n\n## Progress log\r\n"
        );
        assert_eq!(
            append_section(
                "## Goal\nKeep\n\n## Result\nEnd\n",
                "## Decisions",
                "- next"
            ),
            "## Goal\nKeep\n\n## Decisions\n- next\n\n## Result\nEnd\n"
        );
        assert_eq!(
            append_section("## Goal\nKeep", "## Result", "- next"),
            "## Goal\nKeep\n\n## Result\n- next\n"
        );
        assert_eq!(
            rewrite_board("## Result\nEnd\n", "Done", None, "- done"),
            "Stage: Done\n\n## Progress\n- done\n\n## Result\nEnd\n"
        );
    }

    #[test]
    fn entry_text_cannot_inject_headings_or_stage_and_stamp_round_trips() {
        let now = "2026-09-12T14:02:37Z".parse().unwrap();
        let entry = entry_line(
            now,
            "planner",
            "  first\r\n## Injected\r\n\r\nStage: Done\n> quote  ",
        )
        .unwrap();
        assert!(entry.ends_with("@planner: first\n  ## Injected\n\n  Stage: Done\n  > quote"));
        let (stamp, _) = entry.strip_prefix("- ").unwrap().split_once(" @").unwrap();
        assert_eq!(
            Timestamp::strptime(super::super::scratch::PROGRESS_STAMP_FORMAT, stamp).unwrap(),
            now
        );
        let original = "# Board\nStage: Plan (@planner)\n\n## Goal\n\n## Result\nUnchanged\n";
        let board = append_section(original, "## Goal", &entry);
        assert_eq!(
            super::super::scratch::parse_board_stage(&board),
            super::super::scratch::parse_board_stage(original)
        );
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("blackboard.md"), &board).unwrap();
        assert_eq!(
            super::super::scratch::board_section(root.path(), "Goal"),
            Some(entry)
        );
        assert_eq!(
            super::super::scratch::board_section(root.path(), "Result").as_deref(),
            Some("Unchanged")
        );
        assert_eq!(
            super::super::scratch::board_section(root.path(), "Injected"),
            None
        );
        assert_eq!(
            board
                .lines()
                .filter(|line| line.starts_with('#'))
                .collect::<Vec<_>>(),
            original
                .lines()
                .filter(|line| line.starts_with('#'))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            entry_line(now, "user", " \r\n ").unwrap_err().to_string(),
            "provide nonblank text for the board entry"
        );
    }
    #[test]
    fn board_rewrite_preserves_freeform_sections_and_appends_inside_ledger() {
        let board = "# Blackboard\r\nStage:Plan (@planner)\r\n\r\n## Goal\r\nKeep this.\r\n\r\n## Progress log\r\n- old entry\r\n\r\n## Result\r\nUnchanged.\r\n";
        assert_eq!(
            rewrite_board(board, "Implement", Some("coder"), "- next"),
            "# Blackboard\r\nStage: Implement (@coder)\r\n\r\n## Goal\r\nKeep this.\r\n\r\n## Progress log\r\n- old entry\r\n- next\n\r\n## Result\r\nUnchanged.\r\n"
        );
        assert_eq!(
            rewrite_board("Stage: Plan", "Done", None, "- done"),
            "Stage: Done\n\n## Progress\n- done\n"
        );
        assert_eq!(
            rewrite_board("# Board\n", "Done", None, "- done"),
            "# Board\nStage: Done\n\n## Progress\n- done\n"
        );
    }

    #[test]
    fn board_rewrite_handles_empty_ledger_and_unterminated_lines() {
        for (board, expected) in [
            (
                "Stage: Plan\n## Progress log\n\n## Result\n",
                "Stage: Done\n## Progress log\n- done\n\n## Result\n",
            ),
            (
                "Stage: Plan\n## Progress log",
                "Stage: Done\n## Progress log\n- done\n",
            ),
            (
                "Stage: Plan\n## Progress log\n- previous",
                "Stage: Done\n## Progress log\n- previous\n- done\n",
            ),
            (
                "Stage: Plan\nStage: untouched\n",
                "Stage: Done\nStage: untouched\n\n## Progress\n- done\n",
            ),
        ] {
            assert_eq!(rewrite_board(board, "Done", None, "- done"), expected);
        }
    }

    #[test]
    fn ledger_line_round_trips_run_times_to_the_second() {
        let now = "2026-09-12T14:02:37Z".parse().unwrap();
        let line = ledger_line(now, "user", Some("Plan"), "Done", "finished");
        let (stamp, _) = line.strip_prefix("- ").unwrap().split_once(" @").unwrap();
        assert_eq!(
            Timestamp::strptime(super::super::scratch::PROGRESS_STAMP_FORMAT, stamp).unwrap(),
            now
        );
        let worktree = tempfile::tempdir().unwrap();
        std::fs::write(
            worktree.path().join("blackboard.md"),
            format!("Stage: Done\n## Progress\n{line}\n"),
        )
        .unwrap();
        let run = super::super::scratch::board_run(
            worktree.path(),
            &MachineConfig::load_lenient().time_zone(),
        )
        .unwrap();
        assert_eq!(run.started_at, Some(now));
        assert_eq!(run.done_at, Some(now));
    }

    #[test]
    fn multiline_note_cannot_inject_a_board_heading() {
        let line = ledger_line(
            "2026-09-12T14:02:00Z".parse().unwrap(),
            "planner",
            Some("Plan"),
            "Implement",
            "ready\n## Result\r\nStage: Done",
        );
        assert_eq!(line.lines().count(), 1);
        assert!(line.ends_with("@planner: Plan -> Implement — ready ## Result  Stage: Done"));
    }

    #[test]
    fn first_flip_bootstraps_stage_and_progress_without_losing_freeform_text() {
        for (text, expected) in [
            ("", "Stage: Explore (@planner)\n\n## Progress\n- opened\n"),
            (
                "# Work",
                "# Work\nStage: Explore (@planner)\n\n## Progress\n- opened\n",
            ),
            (
                "# Work\n\n## Goal\nFind the bug.\n",
                "# Work\nStage: Explore (@planner)\n\n## Goal\nFind the bug.\n\n## Progress\n- opened\n",
            ),
            (
                "A freeform board.\n",
                "Stage: Explore (@planner)\nA freeform board.\n\n## Progress\n- opened\n",
            ),
            (
                "## Progress\n- existing\n\n## Result\n",
                "Stage: Explore (@planner)\n## Progress\n- existing\n- opened\n\n## Result\n",
            ),
        ] {
            assert_eq!(
                rewrite_board(text, "Explore", Some("planner"), "- opened"),
                expected
            );
        }
        let ledger = ledger_line(
            "2026-09-12T14:02:00Z".parse().unwrap(),
            "planner",
            None,
            "Explore",
            "sweep aimed",
        );
        assert!(ledger.ends_with("@planner: opened Explore — sweep aimed"));
    }
}
