//! Bounded server-log tail scanner for `rimz doctor`.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::Path;

const ENTRY_TEXT_LIMIT: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogSeverity {
    Warn,
    Error,
    Panic,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    pub severity: LogSeverity,
    pub line: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogScan {
    pub size_bytes: u64,
    pub scanned_bytes: u64,
    pub matched: usize,
    pub entries: Vec<LogEntry>,
}

pub fn scan_tail(
    path: &Path,
    window: u64,
    cap: usize,
    classify: impl Fn(&str) -> Option<LogSeverity>,
) -> io::Result<LogScan> {
    let mut file = File::open(path)?;
    let size_bytes = file.metadata()?.len();
    let start = size_bytes.saturating_sub(window);
    file.seek(SeekFrom::Start(start))?;

    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    if start > 0 {
        match buf.iter().position(|byte| *byte == b'\n') {
            Some(pos) => {
                buf.drain(..=pos);
            }
            None => {
                buf.clear();
            }
        }
    }

    let scanned_bytes = size_bytes.saturating_sub(start);
    let mut matched = 0;
    let mut entries = VecDeque::new();
    let text = String::from_utf8_lossy(&buf);
    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r');
        let Some(severity) = classify(line) else {
            continue;
        };
        matched += 1;
        if cap == 0 {
            continue;
        }
        if entries.len() == cap {
            entries.pop_front();
        }
        entries.push_back(LogEntry {
            severity,
            line: truncate_entry(line),
        });
    }

    Ok(LogScan {
        size_bytes,
        scanned_bytes,
        matched,
        entries: entries.into_iter().collect(),
    })
}

fn truncate_entry(line: &str) -> String {
    if line.len() <= ENTRY_TEXT_LIMIT {
        return line.to_owned();
    }
    let boundary = line
        .char_indices()
        .map(|(idx, _)| idx)
        .take_while(|idx| *idx <= ENTRY_TEXT_LIMIT)
        .last()
        .unwrap_or(0);
    format!("{}...", &line[..boundary])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn warn_error(line: &str) -> Option<LogSeverity> {
        if line.starts_with("WARN") {
            Some(LogSeverity::Warn)
        } else if line.starts_with("ERROR") {
            Some(LogSeverity::Error)
        } else {
            None
        }
    }

    #[test]
    fn window_seek_drops_partial_first_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mux.log");
        std::fs::write(
            &path,
            "WARN too old but partly in window\nINFO ok\nERROR recent\n",
        )
        .expect("write log");

        let scan = scan_tail(&path, 24, 10, warn_error).expect("scan");
        assert_eq!(scan.matched, 1);
        assert_eq!(
            scan.entries,
            vec![LogEntry {
                severity: LogSeverity::Error,
                line: "ERROR recent".to_owned(),
            }]
        );
    }

    #[test]
    fn cap_keeps_last_entries_while_matched_counts_all() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mux.log");
        std::fs::write(&path, "WARN one\nERROR two\nWARN three\nERROR four\n").expect("write log");

        let scan = scan_tail(&path, 1024, 2, warn_error).expect("scan");
        assert_eq!(scan.matched, 4);
        assert_eq!(
            scan.entries,
            vec![
                LogEntry {
                    severity: LogSeverity::Warn,
                    line: "WARN three".to_owned(),
                },
                LogEntry {
                    severity: LogSeverity::Error,
                    line: "ERROR four".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn long_lines_truncate_on_char_boundary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mux.log");
        let mut file = File::create(&path).expect("create log");
        writeln!(file, "ERROR {}", "é".repeat(140)).expect("write log");

        let scan = scan_tail(&path, 1024, 10, warn_error).expect("scan");
        assert_eq!(scan.matched, 1);
        assert!(scan.entries[0].line.ends_with("..."));
        assert!(scan.entries[0].line.len() <= ENTRY_TEXT_LIMIT + 3);
        assert!(
            scan.entries[0]
                .line
                .is_char_boundary(scan.entries[0].line.len())
        );
    }
}
