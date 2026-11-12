//! Pure deadline ladder policy shared by the hook consumer and read-only producer.

use jiff::Timestamp;
use std::time::Duration;

use crate::agents::context::TurnSettleOutcome;
use crate::agents::{AgentState, settled_outcome};
use crate::store::run::{RunLifecycle, RunRecord, RunStatus};
use crate::store::snapshot::find_agent;
use crate::utils::time::format_duration_compact;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rung {
    Warn { at: Timestamp, text: String },
    Stop { at: Timestamp },
}

impl Rung {
    pub(super) fn at(&self) -> Timestamp {
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
pub(super) fn due_rung(record: &RunRecord, now: Timestamp) -> Option<Rung> {
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
enum StopChannel {
    Hook,
    Pane,
}

fn stop_channel(kind: &str) -> StopChannel {
    if crate::agents::definition_by_kind(kind)
        .is_ok_and(|agent| agent.spec().capabilities.hook_context.is_some())
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

/// How long an interrupted subagent turn holds its run open for a new turn.
pub(super) const INTERRUPT_WINDOW: Duration = Duration::from_secs(60);

/// Whether interrupt evidence at `at` opens a hold on `record`: a running
/// `rimz subagents` turn run (not a peer or workflow run) with no hold open,
/// whose newest folded `TurnStarted` came before `at`.
pub(super) fn opens_interrupt_hold(record: &RunRecord, at: Timestamp) -> bool {
    record.status == RunStatus::Running
        && holds_interrupts(record)
        && record.interrupted_at.is_none()
        && record.turn_opened_at.is_none_or(|opened| at > opened)
}

/// The runs an interrupt holds open: `rimz subagents` turn runs. Print, peer,
/// team, session and workflow runs fold an interrupt as they always did.
pub(super) fn holds_interrupts(record: &RunRecord) -> bool {
    record.subagent
        && record.lifecycle == RunLifecycle::Turn
        && record.workflow.is_none()
        && record.peer.is_none()
}

/// Whether an interrupt hold has outlived its window with no new turn, and so
/// settles the run canceled. A claimed Stop rung leaves the run to the kill,
/// and so does a due kill.
pub fn interrupt_expired(record: &RunRecord, now: Timestamp) -> bool {
    let stop_claimed = record
        .deadline_at
        .is_some_and(|deadline| record.deadline_notice_at >= Some(deadline));
    record.status == RunStatus::Running
        && record
            .interrupted_at
            .is_some_and(|at| now >= at + INTERRUPT_WINDOW)
        && !stop_claimed
        && !kill_due(record, now)
}

/// The settle instant of an Interrupted marker on `record`'s child card that
/// would open a hold, for the room host to pass to its helper.
pub(super) fn interrupt_evidence(record: &RunRecord, agents: &[AgentState]) -> Option<Timestamp> {
    let child = find_agent(agents, record.kind.as_str(), record.agent_id.as_ref()?)?;
    let context = child.context.as_ref();
    if settled_outcome(child.status, context, child.last_activity)
        != Some(TurnSettleOutcome::Interrupted)
    {
        return None;
    }
    let at = context?.settle?.at;
    opens_interrupt_hold(record, at).then_some(at)
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

    fn held(at: Timestamp) -> RunRecord {
        let mut record = record();
        record.subagent = true;
        record.status = RunStatus::Running;
        record.interrupted_at = Some(at);
        record
    }

    #[test]
    fn a_hold_expires_after_its_window_unless_the_stop_or_the_kill_has_it() {
        let deadline = record().deadline_at.unwrap();
        let at = deadline - Duration::from_secs(600);
        let record = held(at);
        assert!(!interrupt_expired(&record, at + Duration::from_secs(59)));
        assert!(interrupt_expired(&record, at + INTERRUPT_WINDOW));

        let mut claimed = held(deadline + Duration::from_secs(1));
        claimed.deadline_notice_at = claimed.deadline_at;
        let after = deadline + Duration::from_secs(1) + INTERRUPT_WINDOW;
        assert!(!interrupt_expired(&claimed, after), "a claimed stop waits");
        claimed.deadline_notice_at = Some(deadline - Duration::from_secs(180));
        assert!(interrupt_expired(&claimed, after), "a warn claim does not");

        let late = held(deadline + Duration::from_secs(170));
        let kill = kill_at(&late).unwrap();
        assert!(!interrupt_expired(&late, kill + INTERRUPT_WINDOW));

        let mut unheld = held(at);
        unheld.interrupted_at = None;
        assert!(!interrupt_expired(&unheld, at + INTERRUPT_WINDOW));
        let mut settled = held(at);
        settled.status = RunStatus::Canceled;
        assert!(!interrupt_expired(&settled, at + INTERRUPT_WINDOW));
    }

    #[test]
    fn a_child_interrupt_marker_is_evidence_only_for_an_open_unheld_run() {
        let marker: Timestamp = "2026-01-01T00:10:00Z".parse().unwrap();
        let mut child = AgentState::stub("claude", "child", crate::agents::AgentStatus::Running);
        child.last_activity = marker - Duration::from_secs(5);
        let mut context = crate::agents::AgentContext::new("claude", marker);
        context.settle = Some(crate::agents::context::TurnSettle {
            at: marker,
            outcome: crate::agents::context::TurnSettleOutcome::Interrupted,
        });
        child.context = Some(context);
        let mut run = held(marker);
        run.interrupted_at = None;
        run.agent_id = Some(child.agent_id.clone());
        let agents = [child.clone()];
        assert_eq!(interrupt_evidence(&run, &agents), Some(marker));

        let mut stamped = run.clone();
        stamped.interrupted_at = Some(marker);
        assert_eq!(interrupt_evidence(&stamped, &agents), None);
        let mut reopened = run.clone();
        reopened.turn_opened_at = Some(marker);
        assert_eq!(interrupt_evidence(&reopened, &agents), None);
        let mut print = run.clone();
        print.subagent = false;
        assert_eq!(interrupt_evidence(&print, &agents), None);

        let mut active = child.clone();
        active.last_activity = marker;
        assert_eq!(
            interrupt_evidence(&run, &[active]),
            None,
            "work came after it"
        );
        let mut other = child;
        other.agent_id = "other".into();
        assert_eq!(interrupt_evidence(&run, &[other]), None);
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
            "qoder",
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
