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
    summary: String,
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

pub(super) fn collect(mux: MuxName, since: Option<Timestamp>) -> model::MuxLog {
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
) -> model::MuxLog {
    match scan_tail(&path, window, parse_line, diagnose) {
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
            summary: "plugin pane queries timed out — pane discovery lags behind the room"
                .to_owned(),
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
            summary: "a client sent messages zellij could not read — usually a client/server version mismatch"
                .to_owned(),
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
            summary: format!(
                "a pane's configured directory is missing ({cwd}) — zellij started it in the inherited directory"
            ),
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
        summary,
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
        summary: summary.to_owned(),
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
        return Some(expected(
            &format!("action_ack_timeout:{action}"),
            &format!("zellij acknowledged {action} late (the action still ran)"),
            None,
        ));
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
        summary: record.start.message.clone(),
        sample: None,
    })
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

    #[test]
    fn zellij_log_classifier_matches_levels() {
        for (line, expected) in [
            ("Panic occured: unknown messages", Some(LogSeverity::Panic)),
            ("Panic occurred: unknown messages", Some(LogSeverity::Panic)),
            ("ERROR failed to decode", Some(LogSeverity::Error)),
            (
                "ERROR  |zellij_utils::errors::not| 2026-07-17 04:06:02.158 [screen] zellij-utils/src/errors.rs:819: Panic occured:",
                Some(LogSeverity::Panic),
            ),
            ("WARN slow client", Some(LogSeverity::Warn)),
            ("INFO later WARN text is not a level", None),
            ("WARNING is not WARN token", None),
        ] {
            let actual = match parse_zellij_log_line(line) {
                RecordLine::Start(start) => start.severity,
                RecordLine::Continuation => None,
            };
            assert_eq!(actual, expected, "{line}");
        }
    }

    #[test]
    fn zellij_diagnosis_names_a_wrapped_error_by_its_cause() {
        // Zellij prints the same wrapper above every recoverable failure, so two
        // unrelated failures share a header and differ only underneath it.
        let record = |causes: &str| {
            let line = "ERROR  |???                      | 2026-07-17 12:23:34.169 [unnamed] zellij-client/src/lib.rs:975: a non-fatal error occured";
            let RecordLine::Start(start) = parse_zellij_log_line(line) else {
                panic!("record start");
            };
            LogicalRecord {
                start,
                text: format!("{line}\n\nCaused by:\n{causes}"),
                truncated: false,
            }
        };
        let mouse = record("    0: failed to set the cursor shape\n    1: I/O error (os error 5)");
        let write = record("    0: failed to write to the pty\n    1: I/O error (os error 5)");

        let mouse = diagnose_zellij_log_record(None, &mouse, None).unwrap();
        let write = diagnose_zellij_log_record(None, &write, None).unwrap();

        assert_eq!(
            mouse.summary,
            "failed to set the cursor shape: I/O error (os error 5)"
        );
        assert_eq!(mouse.state, LogState::Investigate);
        assert_eq!(mouse.impact, LogImpact::Alarm);
        assert_ne!(
            mouse.key, write.key,
            "two failures under one wrapper stay two issues"
        );
    }

    #[test]
    fn zellij_diagnosis_requires_complete_known_lifecycle_evidence() {
        let unknown_line = "ERROR  |zellij_server::route     | 2026-07-17 12:23:34.169 [server_router] zellij-server/src/route.rs:2642: Received unknown message from client.";
        let RecordLine::Start(unknown_start) = parse_zellij_log_line(unknown_line) else {
            panic!("record start");
        };
        assert_eq!(
            unknown_start.target.as_deref(),
            Some("zellij_server::route")
        );
        assert!(
            unknown_start.at.is_some(),
            "the structured header carries a readable time"
        );
        assert_eq!(
            unknown_start.source.as_deref(),
            Some("zellij-server/src/route.rs:2642")
        );
        let broken_line = "ERROR  |???                      | 2026-07-17 12:23:34.169 [unnamed] zellij-server/src/os_input_output.rs:231: a non-fatal error occured";
        let RecordLine::Start(broken_start) = parse_zellij_log_line(broken_line) else {
            panic!("record start");
        };
        let unknown = LogicalRecord {
            start: unknown_start,
            text: unknown_line.to_owned(),
            truncated: false,
        };
        let broken_pipe = LogicalRecord {
            start: broken_start,
            text: format!(
                "{broken_line}\n\nCaused by:\n    0: failed to send message to client 2\n    1: Broken pipe (os error 32)"
            ),
            truncated: false,
        };

        let expected = diagnose_zellij_log_record(None, &unknown, Some(&broken_pipe)).unwrap();
        assert_eq!(expected.state, LogState::Expected);
        assert_eq!(expected.impact, LogImpact::Info);
        assert!(expected.sample.unwrap().contains("Broken pipe"));
        assert!(diagnose_zellij_log_record(Some(&unknown), &broken_pipe, None).is_none());
        let investigate = diagnose_zellij_log_record(None, &unknown, None).unwrap();
        assert_eq!(investigate.state, LogState::Investigate);
        assert_eq!(investigate.impact, LogImpact::Warn);
        assert!(
            investigate.summary.contains("version mismatch"),
            "an unpaired unknown message names what it usually means: {}",
            investigate.summary
        );

        let RecordLine::Start(start) = parse_zellij_log_line(
            "ERROR  |zellij_server::route| 2026-07-17 12:23:44.875 [server_router] zellij-server/src/route.rs:75: Action CliPipe did not complete within 1s timeout",
        ) else {
            panic!("record start");
        };
        let cli_pipe = LogicalRecord {
            text: start.message.clone(),
            start,
            truncated: false,
        };
        assert_eq!(
            diagnose_zellij_log_record(None, &cli_pipe, None)
                .unwrap()
                .state,
            LogState::Expected
        );
    }

    /// Both records are zellij ERRORs the room provokes on its own, and neither
    /// costs the reader a pane: a pane-targeting action can always lose its target
    /// to a close, and a stale directory still yields a live pane in the inherited
    /// one. Reporting either as an alarm spends the reader's attention on nothing.
    #[test]
    fn zellij_diagnosis_grades_self_inflicted_pane_errors_below_alarm() {
        let record = |line: &str| {
            let RecordLine::Start(start) = parse_zellij_log_line(line) else {
                panic!("record start");
            };
            LogicalRecord {
                text: start.message.clone(),
                start,
                truncated: false,
            }
        };

        let closed = record(
            "ERROR  |zellij_server::screen    | 2026-07-20 00:02:56.758 [screen] zellij-server/src/screen.rs:9730: Pane with id Terminal(336) not found",
        );
        let closed = diagnose_zellij_log_record(None, &closed, None).unwrap();
        assert_eq!(closed.state, LogState::Expected);
        assert_eq!(closed.impact, LogImpact::Info);

        // The id varies per occurrence; one key keeps the race a single issue.
        let other = record(
            "ERROR  |zellij_server::screen    | 2026-07-20 00:02:57.758 [screen] zellij-server/src/screen.rs:9730: Pane with id Terminal(412) not found",
        );
        assert_eq!(
            closed.key,
            diagnose_zellij_log_record(None, &other, None).unwrap().key,
        );

        let cwd = record(
            "ERROR  |zellij_server::os_input_o| 2026-07-19 23:28:54.038 [pty] zellij-server/src/os_input_output_unix.rs:216: Failed to set CWD for new pane. '/tmp/rimz-presence-probe' does not exist or is not a folder",
        );
        let cwd = diagnose_zellij_log_record(None, &cwd, None).unwrap();
        assert_eq!(cwd.state, LogState::Investigate);
        assert_eq!(cwd.impact, LogImpact::Warn);
        assert!(
            cwd.summary.contains("/tmp/rimz-presence-probe"),
            "the directory to fix is the whole point of the line: {}",
            cwd.summary
        );

        // Two stale directories are two fixes, so they stay two issues.
        let elsewhere = record(
            "ERROR  |zellij_server::os_input_o| 2026-07-19 23:28:55.038 [pty] zellij-server/src/os_input_output_unix.rs:216: Failed to set CWD for new pane. '/tmp/gone' does not exist or is not a folder",
        );
        assert_ne!(
            cwd.key,
            diagnose_zellij_log_record(None, &elsewhere, None)
                .unwrap()
                .key,
        );

        // A reworded upstream message keeps its alarm rather than reporting a
        // truncated directory.
        let reworded = record(
            "ERROR  |zellij_server::os_input_o| 2026-07-19 23:28:56.038 [pty] zellij-server/src/os_input_output_unix.rs:216: Failed to set CWD for new pane. '/tmp/gone' is unreadable",
        );
        assert_eq!(
            diagnose_zellij_log_record(None, &reworded, None)
                .unwrap()
                .impact,
            LogImpact::Alarm,
        );
    }

    #[test]
    fn zellij_log_scan_groups_complete_0443_artifacts_conservatively() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("zellij.log");
        std::fs::write(
            &path,
            concat!(
                "ERROR  |zellij_server::route     | 2026-07-17 12:23:34.169 [server_router] zellij-server/src/route.rs:2642: Received unknown message from client.\n",
                "ERROR  |???                      | 2026-07-17 12:23:34.169 [unnamed] zellij-server/src/os_input_output.rs:231: a non-fatal error occured\n",
                "\nCaused by:\n    0: failed to send message to client 2\n    1: Broken pipe (os error 32)\n",
                "INFO   |zellij_server            | 2026-07-17 12:23:35.000 [main] zellij-server/src/lib.rs:1: healthy\n",
                "ERROR  |zellij_server::route     | 2026-07-17 12:23:44.875 [server_router] zellij-server/src/route.rs:75: Action CliPipe did not complete within 1s timeout\n",
                "ERROR  |zellij_server::route     | 2026-07-17 12:23:45.875 [server_router] zellij-server/src/route.rs:75: Action CliPipe did not complete within 1s timeout\n",
                "ERROR  |zellij_server::pty       | 2026-07-17 12:23:46.000 [pty] zellij-server/src/pty.rs:9: pane query failed\n",
                "ERROR  |zellij_utils::errors::not| 2026-07-17 12:23:47.000 [screen] zellij-utils/src/errors.rs:819: Panic occurred:\n",
                "    thread: screen\n    message: fatal\n",
            ),
        )
        .unwrap();

        let scan = scan_tail(
            &path,
            LogWindow {
                bytes: 64 * 1024,
                issue_cap: 10,
                since: None,
            },
            parse_zellij_log_line,
            diagnose_zellij_log_record,
        )
        .unwrap();

        assert_eq!(scan.logical_records, 7);
        assert_eq!(scan.problem_records, 5);
        assert_eq!(scan.issues.len(), 4);
        assert_eq!(scan.issues[0].state, LogState::Expected);
        assert!(scan.issues[0].samples[0].contains("Broken pipe"));
        assert_eq!(scan.issues[1].occurrences, 2);
        assert_eq!(scan.issues[1].state, LogState::Expected);
        assert_eq!(scan.issues[2].state, LogState::Investigate);
        assert_eq!(scan.issues[3].severity, LogSeverity::Panic);
    }

    #[test]
    fn tmux_log_classifier_matches_error_and_fatal_mentions() {
        let classify = |line: &str| match parse_tmux_log_line(line) {
            RecordLine::Start(start) => start.severity,
            RecordLine::Continuation => None,
        };
        assert_eq!(
            classify("server error: client lost"),
            Some(LogSeverity::Error)
        );
        assert_eq!(
            classify("fatal: control socket closed"),
            Some(LogSeverity::Error)
        );
        assert_eq!(
            classify("server panic: invariant failed"),
            Some(LogSeverity::Panic)
        );
        assert_eq!(classify("normal redraw"), None);
    }

    #[test]
    fn tmux_log_lines_carry_epoch_stamp() {
        let stamped = |line: &str| match parse_tmux_log_line(line) {
            RecordLine::Start(start) => start.at,
            RecordLine::Continuation => panic!("record start"),
        };

        assert_eq!(
            stamped("1784493802.501234 server error: client lost"),
            Some(Timestamp::new(1_784_493_802, 501_234_000).unwrap())
        );
        // A line tmux did not stamp survives every `--clear` cutoff rather than
        // being dismissed on a guess.
        assert_eq!(stamped("server error: no stamp here"), None);
    }
}
