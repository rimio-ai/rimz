//! Pure deadline ladder policy shared by the hook consumer and read-only producer.

use jiff::Timestamp;
use std::time::Duration;

use crate::store::run::{RunRecord, RunStatus};
use crate::utils::time::format_duration_compact;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rung {
    Warn { at: Timestamp, text: String },
    Stop { at: Timestamp },
}

impl Rung {
    pub(crate) fn at(&self) -> Timestamp {
        match self {
            Self::Warn { at, .. } | Self::Stop { at } => *at,
        }
    }

    pub fn text(&self) -> String {
        match self {
            Self::Warn { text, .. } => text.clone(),
            Self::Stop { .. } => "Time is up. Stop now and report what is done, what is unverified, and what remains. End your turn.".to_owned(),
        }
    }
}

pub fn kill_at(record: &RunRecord) -> Option<Timestamp> {
    record
        .deadline_at
        .map(|at| at + record.grace.unwrap_or_default())
}

/// The latest crossed rung not yet claimed; `warn` is stored largest offset
/// first, so its rungs come in time order and a missed one is skipped.
pub(crate) fn due_rung(record: &RunRecord, now: Timestamp) -> Option<Rung> {
    if !matches!(record.status, RunStatus::Pending | RunStatus::Running)
        || !kill_at(record).is_some_and(|kill| kill > now)
    {
        return None;
    }
    let deadline = record.deadline_at?;
    let rung = if deadline <= now {
        Rung::Stop { at: deadline }
    } else {
        let (index, offset) = record
            .warn
            .iter()
            .enumerate()
            .rev()
            .find(|(_, offset)| deadline - **offset <= now)?;
        let remaining = Duration::from_secs(deadline.duration_since(now).as_secs().max(0) as u64);
        Rung::Warn {
            at: deadline - *offset,
            text: warning_text(record, remaining, index + 1 == record.warn.len()),
        }
    };
    (Some(rung.at()) > record.deadline_notice_at).then_some(rung)
}

pub fn kill_due(record: &RunRecord, now: Timestamp) -> bool {
    matches!(record.status, RunStatus::Pending | RunStatus::Running)
        && kill_at(record).is_some_and(|deadline| deadline <= now)
}

fn warning_text(record: &RunRecord, remaining: Duration, last: bool) -> String {
    let seconds = remaining.as_secs();
    let remaining = if seconds >= 60 {
        format!("{}m", seconds / 60)
    } else {
        format!("{seconds}s")
    };
    if last {
        return format!(
            "{remaining} left. Wrap up now: finish only what is in flight, then report what is done, what is unverified, and what remains."
        );
    }
    let total = format_duration_compact(record.timeout.unwrap_or_default());
    format!(
        "{remaining} of {total} left. Don't start new investigation; if the task can be finished in a few more steps, finish it, otherwise prepare to report."
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopChannel {
    Hook,
    Pane,
}

pub fn stop_channel(kind: &str) -> StopChannel {
    if crate::agents::definition_by_kind(kind)
        .is_ok_and(|agent| agent.spec().capabilities.hook_context)
    {
        StopChannel::Hook
    } else {
        StopChannel::Pane
    }
}

pub fn stop_due_by_pane(record: &RunRecord, now: Timestamp) -> bool {
    stop_channel(record.kind.as_str()) == StopChannel::Pane
        && matches!(due_rung(record, now), Some(Rung::Stop { .. }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::PermissionMode;
    use crate::ids::{AgentKind, WorkspaceId};
    use crate::store::run::RunStatus;
    use std::time::Duration;

    fn record() -> RunRecord {
        let mut record = RunRecord::new(
            WorkspaceId::from_project_root(std::path::Path::new("/tmp/deadline")),
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "task".into(),
            "/tmp/deadline".into(),
        );
        record.started_at = "2026-01-01T00:00:00Z".parse().unwrap();
        record.timeout = Some(Duration::from_secs(1800));
        record.deadline_at = Some(record.started_at + Duration::from_secs(1800));
        record.grace = Some(Duration::from_secs(180));
        record.warn = vec![Duration::from_secs(360), Duration::from_secs(180)];
        record
    }

    #[test]
    fn latest_crossed_rung_skips_missed_warnings_and_reports_actual_time() {
        let mut record = record();
        let deadline = record.deadline_at.unwrap();
        let first = due_rung(&record, deadline - Duration::from_secs(299)).unwrap();
        assert_eq!(first.at(), deadline - Duration::from_secs(360));
        assert_eq!(
            first.text(),
            "4m of 30m left. Don't start new investigation; if the task can be finished in a few more steps, finish it, otherwise prepare to report."
        );
        record.deadline_notice_at = Some(first.at());
        assert_eq!(due_rung(&record, deadline - Duration::from_secs(200)), None);
        let last = due_rung(&record, deadline - Duration::from_secs(120)).unwrap();
        assert_eq!(
            last.text(),
            "2m left. Wrap up now: finish only what is in flight, then report what is done, what is unverified, and what remains."
        );
        assert!(matches!(
            due_rung(&record, deadline),
            Some(Rung::Stop { .. })
        ));
        record.deadline_notice_at = None;
        assert_eq!(
            due_rung(&record, deadline - Duration::from_secs(120)),
            Some(last)
        );
    }

    #[test]
    fn grace_bounds_delivery_and_terminal_runs_have_no_due_rungs() {
        let mut record = record();
        let deadline = record.deadline_at.unwrap();
        assert_eq!(kill_at(&record), Some(deadline + Duration::from_secs(180)));
        assert!(!kill_due(&record, deadline));
        let kill = deadline + Duration::from_secs(180);
        assert!(kill_due(&record, kill));
        assert_eq!(due_rung(&record, kill), None);
        record.status = RunStatus::Completed;
        assert!(!kill_due(&record, kill));
        assert_eq!(due_rung(&record, deadline), None);
    }

    #[test]
    fn old_records_kill_at_deadline_and_empty_warn_has_only_stop() {
        let mut record = record();
        let deadline = record.deadline_at.unwrap();
        record.warn.clear();
        assert_eq!(due_rung(&record, deadline - Duration::from_secs(1)), None);
        assert_eq!(
            due_rung(&record, deadline),
            Some(Rung::Stop { at: deadline })
        );
        record.grace = None;
        assert_eq!(kill_at(&record), Some(deadline));
        assert_eq!(due_rung(&record, deadline), None);
        assert!(kill_due(&record, deadline));
        record.deadline_at = None;
        assert_eq!(kill_at(&record), None);
        assert_eq!(due_rung(&record, deadline), None);
    }

    #[test]
    fn stop_uses_exactly_one_provider_channel() {
        for kind in [
            "claude", "codex", "copilot", "cursor", "droid", "grok", "qwen",
        ] {
            assert_eq!(stop_channel(kind), StopChannel::Hook, "{kind}");
        }
        for kind in [
            "amp",
            "opencode",
            "pi",
            "antigravity",
            "kimi",
            "kiro",
            "unregistered",
        ] {
            assert_eq!(stop_channel(kind), StopChannel::Pane, "{kind}");
        }
        let mut record = record();
        let deadline = record.deadline_at.unwrap();
        assert!(!stop_due_by_pane(&record, deadline));
        record.kind = AgentKind::new_unchecked("kimi");
        assert!(stop_due_by_pane(&record, deadline));
        record.grace = None;
        assert!(!stop_due_by_pane(&record, deadline));
    }
}
