//! Message delivery assembly, schedule parsing, and pane dispatch.

use std::time::Duration;

use jiff::Timestamp;

pub mod deliver;
pub mod dispatch;
pub(crate) mod fire;
pub mod reply;
pub mod send;

use crate::agents::{AgentState, AgentStatus};
use crate::harness::target::{agent_channel, recipient_channel};
use crate::ids::{AgentKind, AgentSessionId, PaneId, WorkspaceId};
use crate::store::message::{
    AfterCondition, AutoCompact, DeliveryGate, MessageBody, MessageRecord, MessageSender,
    WhenCondition, env_ms, gate_open,
};
use crate::store::snapshot::PaneAgent;
use crate::utils::time::{ClockTime, DurationUnit, parse_duration_units};

const MESSAGE_DURATION_UNITS: &[DurationUnit] = &[
    DurationUnit::Second,
    DurationUnit::Minute,
    DurationUnit::Hour,
    DurationUnit::Day,
];

pub const DEFAULT_SETTLE: Duration = Duration::from_millis(400);
pub const SETTLE_ENV: &str = "RIMZ_MESSAGE_SETTLE_MS";
pub const MESSAGE_WAKE_FILE: &str = "message-wake.json";
/// Default spacing between discrete message pane writes.
pub const DEFAULT_MESSAGE_INTERVAL: Duration = Duration::from_secs(1);
pub const MESSAGE_INTERVAL_ENV: &str = "RIMZ_MESSAGE_INTERVAL_MS";
/// Default gap between raw-typed command segments and before submission,
/// allowing composer paste-burst heuristics to flush.
pub const DEFAULT_COMMAND_SUBMIT_DELAY: Duration = Duration::from_secs(1);
pub const COMMAND_SUBMIT_DELAY_ENV: &str = "RIMZ_MESSAGE_COMMAND_SUBMIT_DELAY_MS";
/// Default cap for unconfirmed `Sent` reconciliation attempts.
pub const DEFAULT_MAX_DELIVERY_ATTEMPTS: u32 = 3;
pub const MAX_DELIVERY_ATTEMPTS_ENV: &str = "RIMZ_MESSAGE_MAX_DELIVERY_ATTEMPTS";

/// Split appended arguments from the adapter's declared compact command while
/// keeping the separating space with the command. This keeps a slash out of a
/// long chunk that a composer may classify as a paste.
fn command_segments<'a>(text: &'a str, command: &str) -> (&'a str, Option<&'a str>) {
    match text
        .strip_prefix(command)
        .and_then(|rest| rest.strip_prefix(' '))
    {
        Some(arguments) if !arguments.is_empty() => (&text[..=command.len()], Some(arguments)),
        _ => (text, None),
    }
}

/// Everything one dispatch decides once and every recipient in the fan-out
/// shares. Text and address vary per recipient and arrive at [`Self::record`].
struct MessageDraft {
    body: MessageBody,
    enter: bool,
    gate: DeliveryGate,
    sender: MessageSender,
    automated: bool,
    force: bool,
    auto_compact: Option<AutoCompact>,
    not_before: Option<Timestamp>,
    after: Vec<AfterCondition>,
    when: Vec<WhenCondition>,
}

enum Recipient<'a> {
    Agent {
        agent: &'a AgentState,
        pane: Option<&'a PaneAgent>,
    },
    Pane {
        pane: &'a PaneAgent,
        bound: Option<&'a AgentState>,
    },
}

struct RecipientIdentity {
    kind: AgentKind,
    agent_id: AgentSessionId,
    agent_name: Option<String>,
    channel: Option<String>,
    pane_id: Option<PaneId>,
}

impl Recipient<'_> {
    fn into_identity(self, scope_channel: Option<&str>) -> RecipientIdentity {
        match self {
            Self::Agent { agent, pane } => RecipientIdentity {
                kind: agent.kind.clone(),
                agent_id: agent.agent_id.clone(),
                agent_name: agent.name.clone(),
                channel: agent_channel(agent).or_else(|| {
                    pane.and_then(|pane| recipient_channel(pane, Some(agent), scope_channel))
                }),
                pane_id: pane.map(|pane| pane.pane_id.clone()),
            },
            Self::Pane { pane, bound } => RecipientIdentity {
                kind: pane.kind.clone(),
                agent_id: bound
                    .map(|agent| agent.agent_id.clone())
                    .or_else(|| pane.agent_id.clone())
                    .unwrap_or_else(|| synthetic_session_for_pane(&pane.pane_id)),
                agent_name: bound
                    .and_then(|agent| agent.name.clone())
                    .or_else(|| pane.name.clone()),
                channel: recipient_channel(pane, bound, scope_channel),
                pane_id: Some(pane.pane_id.clone()),
            },
        }
    }
}

impl MessageDraft {
    /// Stamp this dispatch's decisions onto one recipient's durable record.
    fn record(
        &self,
        workspace_id: WorkspaceId,
        recipient: Recipient<'_>,
        scope_channel: Option<&str>,
        text: &str,
        address: Option<&str>,
    ) -> MessageRecord {
        let recipient = recipient.into_identity(scope_channel);
        let record = MessageRecord::new_for_card(
            workspace_id,
            recipient.kind,
            recipient.agent_id,
            recipient.agent_name,
            text.to_owned(),
            self.enter,
            self.gate,
        )
        .with_body(self.body)
        .with_force(self.force)
        .with_address(address.map(ToOwned::to_owned))
        .with_channel(recipient.channel)
        .with_sender(self.sender.clone())
        .with_automated(self.automated)
        .with_auto_compact(self.auto_compact)
        .with_not_before(self.not_before)
        .with_after(self.after.clone())
        .with_when(self.when.clone());
        match recipient.pane_id {
            Some(pane_id) => record.with_pane_id(pane_id),
            None => record,
        }
    }
}

fn synthetic_session_for_pane(pane_id: &PaneId) -> AgentSessionId {
    let mut rendered = String::from("pane_");
    rendered.extend(pane_id.as_str().chars().map(|ch| match ch {
        'a'..='z' | 'A'..='Z' | '0'..='9' => ch,
        _ => '_',
    }));
    AgentSessionId::from(rendered)
}

pub fn parse_when_status(raw: &str) -> Result<AgentStatus, String> {
    match raw {
        "running" => Ok(AgentStatus::Running),
        "waiting" => Ok(AgentStatus::Waiting),
        "idle" => Ok(AgentStatus::Idle),
        "success" => Ok(AgentStatus::Success),
        "failed" => Ok(AgentStatus::Failed),
        _ => Err(format!(
            "invalid --when status `{raw}`; supported statuses: running, waiting, idle, success, failed"
        )),
    }
}

pub fn parse_when_duration(raw: &str) -> Result<u64, String> {
    let duration = parse_duration_units(raw, MESSAGE_DURATION_UNITS).map_err(|_| {
        format!("invalid --when duration `{raw}`; use a duration like `58m` or `2h`")
    })?;
    if duration.is_zero() {
        return Err("--when duration must be greater than zero".to_owned());
    }
    Ok(duration.as_secs())
}

pub fn gate_open_for_agent(
    gate: DeliveryGate,
    agent: &AgentState,
    force: bool,
    now: Timestamp,
) -> bool {
    !agent.is_compacting(now)
        && (gate_open(gate, agent.effective_status())
            || (force && gate != DeliveryGate::Resume && agent.is_awaiting_input()))
}

pub fn parse_schedule_at(raw: &str, now: &jiff::Zoned) -> Result<Timestamp, String> {
    if let Ok(duration) = parse_duration_units(raw, MESSAGE_DURATION_UNITS) {
        if duration.is_zero() {
            return Err("schedule duration must be greater than zero".to_owned());
        }
        return now
            .checked_add(duration)
            .map(|target| target.timestamp())
            .map_err(|err| format!("schedule `{raw}` cannot be resolved: {err}"));
    }
    let time = raw.parse::<ClockTime>().map_err(|_| {
        format!(
            "invalid schedule `{raw}`; use a duration like `60m` or a 24-hour time like `14:30`"
        )
    })?;
    let candidate = now
        .date()
        .at(time.hour() as i8, time.minute() as i8, 0, 0)
        .to_zoned(now.time_zone().clone())
        .map_err(|err| format!("schedule `{raw}` cannot be resolved today: {err}"))?;
    if candidate.timestamp() > now.timestamp() {
        return Ok(candidate.timestamp());
    }
    now.date()
        .tomorrow()
        .map_err(|err| format!("schedule `{raw}` cannot be resolved tomorrow: {err}"))?
        .at(time.hour() as i8, time.minute() as i8, 0, 0)
        .to_zoned(now.time_zone().clone())
        .map(|target| target.timestamp())
        .map_err(|err| format!("schedule `{raw}` cannot be resolved tomorrow: {err}"))
}

pub fn settle_duration_from_env() -> Duration {
    env_ms(SETTLE_ENV).unwrap_or(DEFAULT_SETTLE)
}

/// Spacing between discrete message pane writes.
pub fn message_interval_from_env() -> Duration {
    env_ms(MESSAGE_INTERVAL_ENV).unwrap_or(DEFAULT_MESSAGE_INTERVAL)
}

/// Gap between raw-typed command text and its submit keystroke.
pub fn command_submit_delay_from_env() -> Duration {
    env_ms(COMMAND_SUBMIT_DELAY_ENV).unwrap_or(DEFAULT_COMMAND_SUBMIT_DELAY)
}

pub fn max_delivery_attempts_from_env() -> u32 {
    std::env::var(MAX_DELIVERY_ATTEMPTS_ENV)
        .ok()
        .and_then(|raw| raw.parse::<u32>().ok())
        .filter(|attempts| *attempts > 0)
        .unwrap_or(DEFAULT_MAX_DELIVERY_ATTEMPTS)
}

#[cfg(test)]
mod tests;
