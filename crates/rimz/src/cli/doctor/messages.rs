use std::collections::BTreeMap;
use std::time::Duration;

use jiff::Timestamp;
use rimz::ledger::event::{EventKind, MessageEventPayload};
use rimz::message::{MessageRecord, MessageStatus};

use super::super::open_ledger;
use super::model::{MessageProblemRow, Messages, OpenCounts, Probe};

const RECENT_FAILURE_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_FAILURE_ROWS: usize = 10;

/// Message queue health: live stuck records and recent terminal delivery
/// failures from the ledger's event log.
pub(super) fn collect_messages(ws: &rimz::ResolvedWorkspace) -> Probe<Messages> {
    let ledger = match open_ledger(ws) {
        Ok(ledger) => ledger,
        Err(err) => {
            return Probe::Unavailable {
                error: err.to_string(),
            };
        }
    };
    let live = match ledger.list_messages() {
        Ok(messages) => messages,
        Err(err) => {
            return Probe::Unavailable {
                error: err.to_string(),
            };
        }
    };
    let mut open = OpenCounts::default();
    let mut stuck = Vec::new();
    for message in &live {
        match message.status {
            MessageStatus::Queued => open.queued += 1,
            MessageStatus::Claimed => open.claimed += 1,
            MessageStatus::Sent => open.sent += 1,
            _ => continue,
        }
        if stuck_open(message) {
            stuck.push(problem_row_from_record(message));
        }
    }

    let events = match ledger.read_events() {
        Ok(events) => events,
        Err(err) => {
            return Probe::Unavailable {
                error: err.to_string(),
            };
        }
    };
    let cutoff = Timestamp::now() - RECENT_FAILURE_WINDOW;
    let mut failures: BTreeMap<String, MessageProblemRow> = BTreeMap::new();
    for event in events {
        let EventKind::Message { payload, .. } = event.kind() else {
            continue;
        };
        if !matches!(
            payload.status,
            MessageStatus::TimedOut | MessageStatus::Errored | MessageStatus::Abandoned
        ) || event.timestamp < cutoff
        {
            continue;
        }
        let row = problem_row_from_event(event.timestamp, payload);
        match failures.get(&row.message_id) {
            Some(existing) if existing.at >= row.at => {}
            _ => {
                failures.insert(row.message_id.clone(), row);
            }
        }
    }
    let mut recent_failures: Vec<_> = failures.into_values().collect();
    recent_failures.sort_by(|left, right| {
        right
            .at
            .cmp(&left.at)
            .then_with(|| right.message_id.cmp(&left.message_id))
    });
    recent_failures.truncate(MAX_FAILURE_ROWS);

    Probe::Ready(Messages {
        open,
        stuck,
        recent_failures,
    })
}

fn stuck_open(message: &MessageRecord) -> bool {
    message.attempts > 1 || message.unconfirmed_sends > 0 || message.last_error.is_some()
}

fn problem_row_from_record(message: &MessageRecord) -> MessageProblemRow {
    MessageProblemRow {
        message_id: message.message_id.to_string(),
        status: message.status.as_str().to_owned(),
        target: target(
            message.agent_name.as_deref(),
            message.kind.as_str(),
            message.agent_id.as_str(),
        ),
        at: message.updated_at,
        problem: problem_text(
            message.attempts,
            message.unconfirmed_sends,
            message.last_error.as_deref(),
        ),
    }
}

fn problem_row_from_event(at: Timestamp, payload: MessageEventPayload) -> MessageProblemRow {
    let MessageEventPayload {
        message_id,
        kind,
        agent_id,
        agent_name,
        status,
        attempts,
        unconfirmed_sends,
        reason,
        ..
    } = payload;
    let problem = reason.unwrap_or_else(|| {
        let detail = problem_text(attempts, unconfirmed_sends, None);
        if detail.is_empty() {
            "no reason recorded".to_owned()
        } else {
            detail
        }
    });
    MessageProblemRow {
        message_id: message_id.to_string(),
        status: status.as_str().to_owned(),
        target: target(agent_name.as_deref(), kind.as_str(), agent_id.as_str()),
        at,
        problem,
    }
}

fn target(agent_name: Option<&str>, kind: &str, agent_id: &str) -> String {
    agent_name
        .filter(|name| !name.is_empty())
        .map(|name| format!("@{name}"))
        .unwrap_or_else(|| format!("{kind}:{agent_id}"))
}

fn problem_text(attempts: u32, unconfirmed_sends: u32, detail: Option<&str>) -> String {
    let mut parts = Vec::new();
    if attempts > 1 {
        parts.push(format!("attempts {attempts}"));
    }
    if unconfirmed_sends > 0 {
        parts.push(format!("unconfirmed sends {unconfirmed_sends}"));
    }
    if let Some(detail) = detail {
        parts.push(if detail.is_empty() {
            "last error recorded".to_owned()
        } else {
            detail.to_owned()
        });
    }
    parts.join(", ")
}
