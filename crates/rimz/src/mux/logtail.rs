//! Bounded logical server-log scanner for `rimz doctor`.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::Path;

use jiff::Timestamp;

const RECORD_TEXT_LIMIT: usize = 8 * 1024;
const SAMPLE_CAP: usize = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogSeverity {
    Warn,
    Error,
    Panic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogState {
    Investigate,
    Expected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogImpact {
    Alarm,
    Warn,
    Info,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogRecordStart {
    pub severity: Option<LogSeverity>,
    /// When the multiplexer wrote this record, once the backend's line format
    /// yields one. Records that carry no readable time survive every cutoff.
    pub at: Option<Timestamp>,
    pub target: Option<String>,
    pub thread: Option<String>,
    pub source: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordLine {
    Start(LogRecordStart),
    Continuation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogicalRecord {
    pub start: LogRecordStart,
    pub text: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogDiagnosis {
    pub key: String,
    pub state: LogState,
    pub impact: LogImpact,
    pub summary: String,
    pub sample: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogIssue {
    pub severity: LogSeverity,
    pub state: LogState,
    pub impact: LogImpact,
    pub summary: String,
    pub occurrences: usize,
    pub first_occurrence: Option<Timestamp>,
    pub last_occurrence: Option<Timestamp>,
    pub samples: Vec<String>,
    pub evidence_truncated: bool,
}

/// How much of the tail to read and which records count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogWindow {
    /// Bytes of the tail to read.
    pub bytes: u64,
    /// Most-recent issue groups to keep; older groups are counted and dropped.
    pub issue_cap: usize,
    /// Ignore records written at or before this moment, so a cleared report
    /// only judges what happened since.
    pub since: Option<Timestamp>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogScan {
    pub size_bytes: u64,
    pub scanned_bytes: u64,
    pub logical_records: usize,
    /// Records the cutoff excluded from diagnosis.
    pub records_before_cutoff: usize,
    pub problem_records: usize,
    pub omitted_issue_groups: usize,
    pub issues: Vec<LogIssue>,
}

pub fn scan_tail(
    path: &Path,
    window: LogWindow,
    parse_line: impl Fn(&str) -> RecordLine,
    diagnose: impl Fn(
        Option<&LogicalRecord>,
        &LogicalRecord,
        Option<&LogicalRecord>,
    ) -> Option<LogDiagnosis>,
) -> io::Result<LogScan> {
    let LogWindow {
        bytes: window_bytes,
        issue_cap: cap,
        since,
    } = window;
    let mut file = File::open(path)?;
    let size_bytes = file.metadata()?.len();
    let start = size_bytes.saturating_sub(window_bytes);
    let starts_mid_line = if start > 0 {
        file.seek(SeekFrom::Start(start - 1))?;
        let mut previous = [0_u8; 1];
        file.read_exact(&mut previous)?;
        previous[0] != b'\n'
    } else {
        false
    };
    file.seek(SeekFrom::Start(start))?;

    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    if starts_mid_line {
        match buf.iter().position(|byte| *byte == b'\n') {
            Some(pos) => {
                buf.drain(..=pos);
            }
            None => buf.clear(),
        }
    }

    let scanned_bytes = size_bytes.saturating_sub(start);
    let text = String::from_utf8_lossy(&buf);
    let mut records = Vec::new();
    let mut current: Option<RecordBuilder> = None;
    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r');
        match parse_line(line) {
            RecordLine::Start(start) => {
                if let Some(record) = current.take() {
                    records.push(record.finish());
                }
                current = Some(RecordBuilder::new(start, line));
            }
            RecordLine::Continuation => {
                if let Some(record) = current.as_mut() {
                    record.push(line);
                }
            }
        }
    }
    if let Some(record) = current {
        records.push(record.finish());
    }

    let logical_records = records.len();
    // A record the cutoff excludes leaves the pool entirely, so neighbour-aware
    // diagnosis never pairs a fresh record with a dismissed one.
    records.retain(|record| {
        record
            .start
            .at
            .zip(since)
            .is_none_or(|(at, since)| at > since)
    });
    let records_before_cutoff = logical_records - records.len();
    let mut problem_records = 0usize;
    let mut groups = Vec::<(String, usize, LogIssue)>::new();
    let mut by_key = HashMap::<String, usize>::new();
    for (record_index, record) in records.iter().enumerate() {
        let Some(diagnosis) = diagnose(
            record_index
                .checked_sub(1)
                .and_then(|prior| records.get(prior)),
            record,
            records.get(record_index + 1),
        ) else {
            continue;
        };
        let Some(severity) = record.start.severity else {
            continue;
        };
        problem_records = problem_records.saturating_add(1);
        let group_key = format!(
            "{:?}:{:?}:{:?}:{}",
            severity, diagnosis.state, diagnosis.impact, diagnosis.key
        );
        if let Some(group_index) = by_key.get(&group_key).copied() {
            groups[group_index].1 = record_index;
            let issue = &mut groups[group_index].2;
            issue.occurrences = issue.occurrences.saturating_add(1);
            if issue.first_occurrence.is_none() {
                issue.first_occurrence = record.start.at;
            }
            if record.start.at.is_some() {
                issue.last_occurrence = record.start.at;
            }
            issue.evidence_truncated |= record.truncated;
            let sample = diagnosis.sample.unwrap_or_else(|| record.text.clone());
            if issue.samples.len() < SAMPLE_CAP && !issue.samples.contains(&sample) {
                issue.samples.push(sample);
            }
            continue;
        }
        let group_index = groups.len();
        by_key.insert(group_key.clone(), group_index);
        groups.push((
            group_key,
            record_index,
            LogIssue {
                severity,
                state: diagnosis.state,
                impact: diagnosis.impact,
                summary: diagnosis.summary,
                occurrences: 1,
                first_occurrence: record.start.at,
                last_occurrence: record.start.at,
                samples: vec![diagnosis.sample.unwrap_or_else(|| record.text.clone())],
                evidence_truncated: record.truncated,
            },
        ));
    }

    groups.sort_by_key(|(_, last_index, _)| *last_index);
    let omitted_issue_groups = groups.len().saturating_sub(cap);
    let issues = if cap == 0 {
        Vec::new()
    } else {
        groups
            .into_iter()
            .skip(omitted_issue_groups)
            .map(|(_, _, issue)| issue)
            .collect()
    };
    Ok(LogScan {
        size_bytes,
        scanned_bytes,
        logical_records,
        records_before_cutoff,
        problem_records,
        omitted_issue_groups,
        issues,
    })
}

struct RecordBuilder {
    start: LogRecordStart,
    text: String,
    truncated: bool,
}

impl RecordBuilder {
    fn new(start: LogRecordStart, line: &str) -> Self {
        let mut builder = Self {
            start,
            text: String::new(),
            truncated: false,
        };
        builder.push(line);
        builder
    }

    fn push(&mut self, line: &str) {
        if self.truncated {
            return;
        }
        if !self.text.is_empty() {
            self.push_text("\n");
        }
        self.push_text(line);
    }

    fn push_text(&mut self, value: &str) {
        let remaining = RECORD_TEXT_LIMIT.saturating_sub(self.text.len());
        if value.len() <= remaining {
            self.text.push_str(value);
            return;
        }
        let boundary = value
            .char_indices()
            .map(|(index, _)| index)
            .take_while(|index| *index <= remaining)
            .last()
            .unwrap_or(0);
        self.text.push_str(&value[..boundary]);
        self.truncated = true;
    }

    fn finish(self) -> LogicalRecord {
        LogicalRecord {
            start: self.start,
            text: self.text,
            truncated: self.truncated,
        }
    }
}

pub fn normalized_issue_key(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut in_digits = false;
    let mut in_space = false;
    for ch in value.chars() {
        if ch.is_ascii_digit() {
            if !in_digits {
                normalized.push('#');
            }
            in_digits = true;
            in_space = false;
        } else if ch.is_whitespace() {
            if !in_space {
                normalized.push(' ');
            }
            in_space = true;
            in_digits = false;
        } else {
            normalized.push(ch.to_ascii_lowercase());
            in_digits = false;
            in_space = false;
        }
    }
    normalized.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(bytes: u64, issue_cap: usize) -> LogWindow {
        LogWindow {
            bytes,
            issue_cap,
            since: None,
        }
    }

    /// `WARN@5 text` stamps the record at second 5 of the epoch.
    fn parse(line: &str) -> RecordLine {
        let (severity, rest) = if let Some(rest) = line.strip_prefix("INFO") {
            (None, rest)
        } else if let Some(rest) = line.strip_prefix("WARN") {
            (Some(LogSeverity::Warn), rest)
        } else if let Some(rest) = line.strip_prefix("ERROR") {
            (Some(LogSeverity::Error), rest)
        } else {
            return RecordLine::Continuation;
        };
        let (at, message) = match rest.strip_prefix('@') {
            Some(rest) => {
                let (secs, message) = rest.split_once(' ').unwrap();
                (
                    Some(Timestamp::from_second(secs.parse().unwrap()).unwrap()),
                    message,
                )
            }
            None => (None, rest.strip_prefix(' ').unwrap_or(rest)),
        };
        RecordLine::Start(LogRecordStart {
            severity,
            at,
            message: message.to_owned(),
            ..LogRecordStart::default()
        })
    }

    fn diagnose(
        _previous: Option<&LogicalRecord>,
        record: &LogicalRecord,
        _next: Option<&LogicalRecord>,
    ) -> Option<LogDiagnosis> {
        record.start.severity.map(|_| LogDiagnosis {
            key: normalized_issue_key(&record.start.message),
            state: LogState::Investigate,
            impact: LogImpact::Warn,
            summary: record.start.message.clone(),
            sample: None,
        })
    }

    #[test]
    fn assembles_records_and_non_problem_start_terminates_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mux.log");
        std::fs::write(
            &path,
            "orphan continuation\nWARN first\nCaused by: detail\n\nINFO ok\nERROR second\n",
        )
        .unwrap();

        let scan = scan_tail(&path, window(1024, 10), parse, diagnose).unwrap();
        assert_eq!(scan.logical_records, 3);
        assert_eq!(scan.problem_records, 2);
        assert_eq!(scan.issues[0].samples[0], "WARN first\nCaused by: detail\n");
    }

    #[test]
    fn groups_before_cap_and_keeps_one_sample() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mux.log");
        std::fs::write(
            &path,
            "WARN client 1\nWARN client 2\nERROR other\nWARN client 3\n",
        )
        .unwrap();

        let scan = scan_tail(&path, window(1024, 1), parse, diagnose).unwrap();
        assert_eq!(scan.problem_records, 4);
        assert_eq!(scan.omitted_issue_groups, 1);
        assert_eq!(scan.issues.len(), 1);
    }

    #[test]
    fn cutoff_judges_only_records_written_after_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mux.log");
        std::fs::write(&path, "WARN@10 dismissed\nWARN@20 kept\nWARN undated\n").unwrap();

        let scan = scan_tail(
            &path,
            LogWindow {
                bytes: 1024,
                issue_cap: 10,
                since: Some(Timestamp::from_second(15).unwrap()),
            },
            parse,
            diagnose,
        )
        .unwrap();

        assert_eq!(scan.logical_records, 3);
        assert_eq!(scan.records_before_cutoff, 1);
        assert_eq!(scan.problem_records, 2, "undated records survive a cutoff");
        let summaries: Vec<_> = scan
            .issues
            .iter()
            .map(|issue| issue.summary.as_str())
            .collect();
        assert_eq!(summaries, ["kept", "undated"]);
        assert_eq!(
            scan.issues[0].first_occurrence,
            Some(Timestamp::from_second(20).unwrap())
        );
    }

    #[test]
    fn window_seek_drops_partial_initial_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mux.log");
        std::fs::write(&path, "WARN too old\ndetail\nINFO boundary\nERROR recent\n").unwrap();

        let scan = scan_tail(&path, window(27, 10), parse, diagnose).unwrap();
        assert_eq!(scan.problem_records, 1);
        assert_eq!(scan.issues[0].summary, "recent");
    }

    #[test]
    fn window_seek_keeps_record_when_start_is_line_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mux.log");
        let recent = "ERROR recent\n";
        std::fs::write(&path, format!("WARN old\n{recent}")).unwrap();

        let scan = scan_tail(&path, window(recent.len() as u64, 10), parse, diagnose).unwrap();

        assert_eq!(scan.problem_records, 1);
        assert_eq!(scan.issues[0].summary, "recent");
    }

    #[test]
    fn record_truncation_is_utf8_safe() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mux.log");
        std::fs::write(&path, format!("ERROR {}\n", "é".repeat(RECORD_TEXT_LIMIT))).unwrap();

        let scan = scan_tail(&path, window(32 * 1024, 10), parse, diagnose).unwrap();
        assert!(scan.issues[0].evidence_truncated);
        assert!(scan.issues[0].samples[0].is_char_boundary(scan.issues[0].samples[0].len()));
    }
}
