//! Private multiplexer log collection for `rimz doctor`.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use rimz::ids::MuxName;
use rimz::mux::{tmux, zellij};

use super::model;

const RECORD_TEXT_LIMIT: usize = 8 * 1024;
const SAMPLE_CAP: usize = 1;
const WINDOW_BYTES: u64 = 256 * 1024;
/// Issue groups to keep from the tail. Routine lifecycle groups share one
/// rendered line, so the budget buys real findings rather than repetition.
const ISSUE_CAP: usize = 24;

/// Whether the report may carry text copied out of a multiplexer log record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LogText {
    Include,
    Omit,
}

/// Where a summary's words came from, so `--no-log-text` has one decision point.
#[derive(Clone, Debug, PartialEq, Eq)]
enum LogSummary {
    /// RimZ's own sentence about a record it recognized; carries nothing from the log.
    Authored(String),
    /// A summary whose words came from the record, and its log-free alternative.
    FromLog { text: String, without_text: String },
}

impl LogSummary {
    fn resolve(self, mode: LogText) -> String {
        match (self, mode) {
            (Self::Authored(text), _) | (Self::FromLog { text, .. }, LogText::Include) => text,
            (Self::FromLog { without_text, .. }, LogText::Omit) => without_text,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LogSeverity {
    Warn,
    Error,
    Panic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LogState {
    Investigate,
    Expected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LogImpact {
    Alarm,
    Warn,
    Info,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct LogRecordStart {
    severity: Option<LogSeverity>,
    /// When the multiplexer wrote this record, once the backend's line format
    /// yields one. Records that carry no readable time survive every cutoff.
    at: Option<Timestamp>,
    target: Option<String>,
    source: Option<String>,
    message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RecordLine {
    Start(LogRecordStart),
    Continuation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LogicalRecord {
    start: LogRecordStart,
    text: String,
    truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LogDiagnosis {
    key: String,
    state: LogState,
    impact: LogImpact,
    summary: LogSummary,
    sample: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LogIssue {
    severity: LogSeverity,
    state: LogState,
    impact: LogImpact,
    summary: String,
    occurrences: usize,
    first_occurrence: Option<Timestamp>,
    last_occurrence: Option<Timestamp>,
    samples: Vec<String>,
    evidence_truncated: bool,
}

/// How much of the tail to read and which records count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LogWindow {
    /// Bytes of the tail to read.
    bytes: u64,
    /// Most-recent issue groups to keep; older groups are counted and dropped.
    issue_cap: usize,
    /// Ignore records written at or before this moment, so a cleared report
    /// only judges what happened since.
    since: Option<Timestamp>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LogScan {
    size_bytes: u64,
    scanned_bytes: u64,
    logical_records: usize,
    /// Records the cutoff excluded from diagnosis.
    records_before_cutoff: usize,
    problem_records: usize,
    omitted_issue_groups: usize,
    issues: Vec<LogIssue>,
}

pub(super) fn collect(mux: MuxName, since: Option<Timestamp>, log_text: LogText) -> model::MuxLog {
    let window = LogWindow {
        bytes: WINDOW_BYTES,
        issue_cap: ISSUE_CAP,
        since,
    };
    match mux {
        MuxName::Zellij => {
            let path = zellij::log_file();
            match path.try_exists() {
                Ok(true) => scan(
                    path,
                    model::LogScope::HostUser {
                        uid: nix::unistd::Uid::current().as_raw(),
                    },
                    window,
                    parse_zellij_log_line,
                    diagnose_zellij_log_record,
                    log_text,
                ),
                Ok(false) => model::MuxLog::Missing {
                    path: path.display().to_string(),
                },
                Err(err) => model::MuxLog::Unavailable {
                    error: format!("{}: {err}", path.display()),
                },
            }
        }
        MuxName::Tmux => match tmux::server_log_file() {
            Some(path) => scan(
                path,
                model::LogScope::Server,
                window,
                parse_tmux_log_line,
                diagnose_tmux_log_record,
                log_text,
            ),
            None => model::MuxLog::Disabled {
                hint: "server logging off (start tmux with `-v` to enable)".to_owned(),
            },
        },
    }
}

fn scan(
    path: PathBuf,
    scope: model::LogScope,
    window: LogWindow,
    parse_line: fn(&str) -> RecordLine,
    diagnose: fn(
        Option<&LogicalRecord>,
        &LogicalRecord,
        Option<&LogicalRecord>,
    ) -> Option<LogDiagnosis>,
    log_text: LogText,
) -> model::MuxLog {
    match scan_tail(&path, window, parse_line, diagnose, log_text) {
        Ok(scan) => model::MuxLog::Ready {
            path: path.display().to_string(),
            scope,
            size_bytes: scan.size_bytes,
            scanned_bytes: scan.scanned_bytes,
            logical_records: scan.logical_records,
            records_before_cutoff: scan.records_before_cutoff,
            since: window.since,
            problem_records: scan.problem_records,
            omitted_issue_groups: scan.omitted_issue_groups,
            log_text_omitted: log_text == LogText::Omit,
            issues: scan
                .issues
                .into_iter()
                .map(|issue| model::MuxLogIssue {
                    source_severity: severity_label(issue.severity).to_owned(),
                    state: match issue.state {
                        LogState::Investigate => model::DoctorState::Investigate,
                        LogState::Expected => model::DoctorState::Expected,
                    },
                    impact: match issue.impact {
                        LogImpact::Alarm => model::DoctorImpact::Alarm,
                        LogImpact::Warn => model::DoctorImpact::Warn,
                        LogImpact::Info => model::DoctorImpact::Info,
                    },
                    summary: issue.summary,
                    occurrences: issue.occurrences,
                    first_occurrence: issue.first_occurrence,
                    last_occurrence: issue.last_occurrence,
                    samples: issue.samples,
                    evidence_truncated: issue.evidence_truncated,
                })
                .collect(),
        },
        Err(err) => model::MuxLog::Unavailable {
            error: format!("{}: {err}", path.display()),
        },
    }
}

fn severity_label(severity: LogSeverity) -> &'static str {
    match severity {
        LogSeverity::Warn => "warn",
        LogSeverity::Error => "error",
        LogSeverity::Panic => "panic",
    }
}

fn scan_tail(
    path: &Path,
    window: LogWindow,
    parse_line: impl Fn(&str) -> RecordLine,
    diagnose: impl Fn(
        Option<&LogicalRecord>,
        &LogicalRecord,
        Option<&LogicalRecord>,
    ) -> Option<LogDiagnosis>,
    log_text: LogText,
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
            if log_text == LogText::Include {
                issue.evidence_truncated |= record.truncated;
                let sample = diagnosis.sample.unwrap_or_else(|| record.text.clone());
                if issue.samples.len() < SAMPLE_CAP && !issue.samples.contains(&sample) {
                    issue.samples.push(sample);
                }
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
                summary: diagnosis.summary.resolve(log_text),
                occurrences: 1,
                first_occurrence: record.start.at,
                last_occurrence: record.start.at,
                samples: if log_text == LogText::Include {
                    vec![diagnosis.sample.unwrap_or_else(|| record.text.clone())]
                } else {
                    Vec::new()
                },
                evidence_truncated: log_text == LogText::Include && record.truncated,
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

fn normalized_issue_key(value: &str) -> String {
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

fn parse_zellij_log_line(line: &str) -> RecordLine {
    if line.starts_with("Panic occured") || line.starts_with("Panic occurred") {
        return RecordLine::Start(LogRecordStart {
            severity: Some(LogSeverity::Panic),
            message: line.to_owned(),
            ..LogRecordStart::default()
        });
    }
    let Some((severity_name, rest)) = ["TRACE", "DEBUG", "INFO", "WARN", "ERROR"]
        .into_iter()
        .find_map(|severity| {
            line.strip_prefix(severity)
                .filter(|rest| rest.chars().next().is_none_or(char::is_whitespace))
                .map(|rest| (severity, rest))
        })
    else {
        return RecordLine::Continuation;
    };
    let mut severity = match severity_name {
        "WARN" => Some(LogSeverity::Warn),
        "ERROR" => Some(LogSeverity::Error),
        _ => None,
    };
    if let Some(header) = parse_zellij_structured_header(rest) {
        if header.message.starts_with("Panic occured")
            || header.message.starts_with("Panic occurred")
        {
            severity = Some(LogSeverity::Panic);
        }
        return RecordLine::Start(LogRecordStart {
            severity,
            at: parse_zellij_timestamp(&header.timestamp),
            target: Some(header.target),
            source: Some(header.source),
            message: header.message,
        });
    }
    let message = rest.trim_start().to_owned();
    RecordLine::Start(LogRecordStart {
        severity,
        message,
        ..LogRecordStart::default()
    })
}

/// Zellij stamps each record with local wall-clock time and no offset
/// (`2026-07-19 13:37:49.089`), so the machine's own zone resolves it.
fn parse_zellij_timestamp(raw: &str) -> Option<Timestamp> {
    raw.replace(' ', "T")
        .parse::<jiff::civil::DateTime>()
        .ok()?
        .to_zoned(jiff::tz::TimeZone::system())
        .ok()
        .map(|zoned| zoned.timestamp())
}

struct ZellijLogHeader {
    target: String,
    timestamp: String,
    source: String,
    message: String,
}

fn parse_zellij_structured_header(rest: &str) -> Option<ZellijLogHeader> {
    let rest = rest.trim_start().strip_prefix('|')?;
    let (target, rest) = rest.split_once('|')?;
    let (timestamp, rest) = rest.trim_start().split_once(" [")?;
    let (thread, rest) = rest.split_once(']')?;
    let (source, message) = rest.trim_start().split_once(": ")?;
    let target = target.trim();
    let timestamp = timestamp.trim();
    let thread = thread.trim();
    let source = source.trim();
    if target.is_empty() || timestamp.is_empty() || thread.is_empty() || source.is_empty() {
        return None;
    }
    Some(ZellijLogHeader {
        target: target.to_owned(),
        timestamp: timestamp.to_owned(),
        source: source.to_owned(),
        message: message.trim_end().to_owned(),
    })
}

/// The wrapper zellij prints above every recoverable failure; it names nothing
/// on its own, so the `Caused by:` chain underneath is the real subject.
const NON_FATAL_HEADER: &str = "a non-fatal error occured";

fn diagnose_zellij_log_record(
    previous: Option<&LogicalRecord>,
    record: &LogicalRecord,
    next: Option<&LogicalRecord>,
) -> Option<LogDiagnosis> {
    let severity = record.start.severity?;
    // A disconnect writes two records; the second rides with the first.
    if previous.is_some_and(is_unknown_client_message) && is_client_send_failure(record) {
        return None;
    }
    let paired_send_failure =
        next.filter(|next| is_unknown_client_message(record) && is_client_send_failure(next));

    let target = record.start.target.as_deref().unwrap_or_default();
    let message = record.start.message.trim();
    let causes = record_causes(&record.text);
    let subject = match (message.starts_with(NON_FATAL_HEADER), causes.first()) {
        (true, Some(cause)) => cause.as_str(),
        _ => message,
    };

    if let Some(expected) = expected_zellij_lifecycle(record, subject, paired_send_failure) {
        return Some(expected);
    }

    // The sidebar reads panes through these plugin calls, so a timeout here is
    // the log's own account of pane discovery falling behind.
    if subject.contains("timed out") && subject.contains("for plugin") {
        return Some(LogDiagnosis {
            key: "plugin_pane_query_timeout".to_owned(),
            state: LogState::Investigate,
            impact: LogImpact::Warn,
            summary: LogSummary::Authored(
                "plugin pane queries timed out — pane discovery lags behind the room".to_owned(),
            ),
            sample: None,
        });
    }
    // An unknown client message with no disconnect behind it, and the logout
    // zellij escalates to, are the same event stream: a client speaking a
    // protocol this server does not know.
    if is_unknown_client_message(record)
        || (message.starts_with("Client sent over") && message.contains("unknown messages"))
    {
        return Some(LogDiagnosis {
            key: "client_protocol_mismatch".to_owned(),
            state: LogState::Investigate,
            impact: LogImpact::Warn,
            summary: LogSummary::Authored("a client sent messages zellij could not read — usually a client/server version mismatch"
                .to_owned()),
            sample: None,
        });
    }
    // Zellij keeps the pane and spawns it in the inherited directory, so the
    // pane lives and only its directory is wrong. Keying on the path keeps two
    // different stale directories in two groups, each naming its own fix.
    if let Some(cwd) = missing_pane_cwd(subject) {
        return Some(LogDiagnosis {
            key: normalized_issue_key(&format!("missing_pane_cwd:{cwd}")),
            state: LogState::Investigate,
            impact: LogImpact::Warn,
            summary: LogSummary::FromLog {
                text: format!(
                    "a pane's configured directory is missing ({cwd}) — zellij started it in the inherited directory"
                ),
                without_text: "a pane's configured directory is missing — zellij started it in the inherited directory".to_owned(),
            },
            sample: None,
        });
    }

    let impact = match severity {
        LogSeverity::Warn => LogImpact::Warn,
        LogSeverity::Error | LogSeverity::Panic => LogImpact::Alarm,
    };
    // Naming the whole cause chain keeps unrelated failures in separate groups;
    // keyed on the wrapper alone they collapse into one meaningless bucket.
    let summary = if causes.is_empty() || !message.starts_with(NON_FATAL_HEADER) {
        message.to_owned()
    } else {
        causes.join(": ")
    };
    Some(LogDiagnosis {
        key: normalized_issue_key(&format!("{target}:{summary}")),
        state: LogState::Investigate,
        impact,
        summary: LogSummary::FromLog {
            text: summary,
            without_text: if target.is_empty() {
                format!("an unclassified {} record", severity_label(severity))
            } else {
                format!(
                    "an unclassified {} record from {target}",
                    severity_label(severity)
                )
            },
        },
        sample: None,
    })
}

/// Log traffic the room provokes by living its normal life: clients attaching
/// and leaving, panes closing, a busy server acknowledging late. Each one reads
/// as an ERROR in zellij's log and means nothing to the operator.
fn expected_zellij_lifecycle(
    record: &LogicalRecord,
    subject: &str,
    paired_send_failure: Option<&LogicalRecord>,
) -> Option<LogDiagnosis> {
    let expected = |key: &str, summary: &str, sample: Option<String>| LogDiagnosis {
        key: key.to_owned(),
        state: LogState::Expected,
        impact: LogImpact::Info,
        summary: LogSummary::Authored(summary.to_owned()),
        sample,
    };

    // Only the proven pair reads as a departure: an unknown client message on
    // its own is evidence of something else, and gets to keep saying so.
    if paired_send_failure.is_some() || is_client_send_failure(record) {
        return Some(expected(
            "client_disconnect",
            "a client left the session",
            paired_send_failure.map(|next| format!("{}\n{}", record.text, next.text)),
        ));
    }
    if let Some(action) = action_ack_timeout(subject) {
        return Some(LogDiagnosis {
            key: format!("action_ack_timeout:{action}"),
            state: LogState::Expected,
            impact: LogImpact::Info,
            summary: LogSummary::FromLog {
                text: format!("zellij acknowledged {action} late (the action still ran)"),
                without_text: "zellij acknowledged an action late (the action still ran)"
                    .to_owned(),
            },
            sample: None,
        });
    }
    // Zellij truncates the target column, so the untruncated source path is the
    // reliable way to place a record in the server's pty reader.
    let source = record.start.source.as_deref().unwrap_or_default();
    if source.contains("terminal_bytes.rs") && subject.contains("I/O error (os error 5)") {
        return Some(expected(
            "closed_pane_pty",
            "read from a closed pane's terminal",
            None,
        ));
    }
    if subject.starts_with("failed to disable mouse mode") {
        return Some(expected(
            "client_teardown_mouse_mode",
            "a client tore down mouse mode on a terminal already gone",
            None,
        ));
    }
    // Pane-targeting actions name a pane the room listed a moment earlier, so a
    // pane that closes inside that window resolves to nothing. The id varies per
    // occurrence and one key groups them, because the race is the single fact.
    if subject.starts_with("Pane with id") && subject.ends_with("not found") {
        return Some(expected(
            "closed_pane_action",
            "addressed a pane that had already closed",
            None,
        ));
    }
    let lower = record.text.to_ascii_lowercase();
    if lower.contains("closed terminal") && lower.contains("resize") && lower.contains("caused by")
    {
        return Some(expected(
            "closed_terminal_resize",
            "resized a pane whose terminal had closed",
            None,
        ));
    }
    None
}

/// The directory a pane asked for and zellij could not enter, from
/// `Failed to set CWD for new pane. '<path>' does not exist or is not a folder`.
/// Matching the whole wording keeps a reworded upstream message falling through
/// to the generic path rather than reporting a truncated directory.
fn missing_pane_cwd(subject: &str) -> Option<&str> {
    subject
        .strip_prefix("Failed to set CWD for new pane. '")?
        .strip_suffix("' does not exist or is not a folder")
}

/// The action zellij took too long to acknowledge, from
/// `Action CliPipe did not complete within 1s timeout`.
fn action_ack_timeout(subject: &str) -> Option<&str> {
    subject
        .strip_prefix("Action ")?
        .split_once(" did not complete within")
        .map(|(action, _)| action)
}

fn is_unknown_client_message(record: &LogicalRecord) -> bool {
    record.start.message == "Received unknown message from client."
}

fn is_client_send_failure(record: &LogicalRecord) -> bool {
    record.start.message.starts_with(NON_FATAL_HEADER)
        && record.text.contains("failed to send message to client")
        && record.text.contains("Broken pipe (os error 32)")
}

/// The `Caused by:` chain under an error record, outermost cause first. Anyhow
/// numbers the entries once there is more than one; a lone cause is bare.
fn record_causes(text: &str) -> Vec<String> {
    text.lines()
        .skip_while(|line| line.trim() != "Caused by:")
        .skip(1)
        .map(str::trim)
        .take_while(|line| !line.is_empty())
        .map(|line| strip_cause_index(line).trim().to_owned())
        .collect()
}

/// Drop anyhow's `0: ` ordinal, keeping the cause text itself.
fn strip_cause_index(line: &str) -> &str {
    line.split_once(' ')
        .filter(|(ordinal, _)| {
            ordinal.ends_with(':')
                && ordinal
                    .trim_end_matches(':')
                    .chars()
                    .all(|ch| ch.is_ascii_digit())
        })
        .map_or(line, |(_, rest)| rest)
}

fn parse_tmux_log_line(line: &str) -> RecordLine {
    if line.is_empty() || line.starts_with([' ', '\t']) {
        return RecordLine::Continuation;
    }
    let lower = line.to_ascii_lowercase();
    let severity = if lower.contains("panic") {
        Some(LogSeverity::Panic)
    } else if lower.contains("fatal") || lower.contains("error") {
        Some(LogSeverity::Error)
    } else {
        None
    };
    RecordLine::Start(LogRecordStart {
        severity,
        at: line
            .split_whitespace()
            .next()
            .and_then(parse_tmux_timestamp),
        message: line.to_owned(),
        ..LogRecordStart::default()
    })
}

/// tmux opens every log line with `<seconds>.<microseconds>` since the epoch.
fn parse_tmux_timestamp(token: &str) -> Option<Timestamp> {
    let (seconds, micros) = token.split_once('.')?;
    let micros: i64 = format!("{micros:0<6}").get(..6)?.parse().ok()?;
    Timestamp::new(seconds.parse().ok()?, i32::try_from(micros).ok()? * 1_000).ok()
}

fn diagnose_tmux_log_record(
    _previous: Option<&LogicalRecord>,
    record: &LogicalRecord,
    _next: Option<&LogicalRecord>,
) -> Option<LogDiagnosis> {
    let severity = record.start.severity?;
    Some(LogDiagnosis {
        key: normalized_issue_key(&record.start.message),
        state: LogState::Investigate,
        impact: if severity == LogSeverity::Panic {
            LogImpact::Alarm
        } else {
            LogImpact::Warn
        },
        summary: LogSummary::FromLog {
            text: record.start.message.clone(),
            without_text: format!("an unclassified {} record", severity_label(severity)),
        },
        sample: None,
    })
}

#[cfg(test)]
mod tests;
