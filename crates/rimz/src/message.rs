//! Message delivery assembly, schedule parsing, and pane dispatch.

use std::time::Duration;

use jiff::Timestamp;

use crate::harness::auto_continue::CardStatus;
use crate::store::snapshot::SidebarSnapshot;

pub mod announce;
pub mod compact;
pub mod deliver;
pub mod dispatch;
pub(crate) mod fire;
mod premise;
pub mod send;
pub mod soft_compact;
pub mod synthetic;
pub mod turn_wait;

/// Delivery timing shared by dispatch and pane writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryKind {
    Boundary,
    Steer,
    Interrupt,
}

/// Why native pane input is reserved, even when attention still reads running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputWait {
    Human,
    PermissionReview,
    TurnEnded,
}

impl InputWait {
    fn for_agent(agent: &AgentState, now: Timestamp) -> Self {
        if agent.is_reviewing_permission(now) {
            Self::PermissionReview
        } else if agent.status == AgentStatus::Waiting && !agent.holds_open_turn() {
            Self::TurnEnded
        } else {
            Self::Human
        }
    }
}

impl std::fmt::Display for InputWait {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Human => "is waiting on your input in its pane; answer it or pass --force",
            Self::PermissionReview => concat!(
                "has a permission request under the provider's automatic review; ",
                "its pane may be busy; retry shortly or pass --force"
            ),
            Self::TurnEnded => concat!(
                "ended its turn on a provider error, but a background approval may still be ",
                "open in its pane; check the pane or pass --force"
            ),
        })
    }
}

/// Transient stop evidence, independent of the durable message receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterruptConfirmation {
    /// No interrupt key was pressed: the receiver was resting, or the send was no interrupt.
    NotChecked,
    /// The receiver's row showed the stop before the text was typed.
    Confirmed,
    /// No stop showed within `window`; the text was typed anyway.
    NotConfirmed { window: Duration },
}

impl InterruptConfirmation {
    /// The sender's warning for a stop that did not show, as every surface words it; `label` is
    /// the receiver's address in the command it names.
    pub fn warning(self, label: &str) -> Option<String> {
        let Self::NotConfirmed { window } = self else {
            return None;
        };
        Some(format!(
            "interrupt not confirmed within {} ms, so the message was typed anyway; its turn may still be running (rimz pane capture {label}), or its stop went unreported, as from an untrusted Codex Interrupt hook (rimz doctor)",
            window.as_millis()
        ))
    }
}

/// Why an interrupt's text was withheld, as every surface words it: whether its key went
/// (`pressed`), the `cause`, and the command that sends the text.
pub fn interrupt_withheld_detail(
    cause: &str,
    pressed: bool,
    message_id: &crate::ids::MessageId,
) -> String {
    if pressed {
        format!("interrupted, text not sent: {cause}; send it: rimz message steer {message_id}")
    } else {
        format!("not interrupted: {cause}; send now: rimz message interrupt {message_id}")
    }
}

use crate::address::recipient_channel;
use crate::agents::{AgentCardRef, AgentState, AgentStatus, RefreshSpawn};
use crate::ids::{AgentKind, AgentSessionId, LaneName, PaneId, WorkspaceId};
use crate::store::message::{
    AfterCondition, AutoCompact, DeliveryGate, MessageBody, MessageRecord, MessageSender, Premise,
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

const DEFAULT_SETTLE: Duration = Duration::from_millis(400);
const SETTLE_ENV: &str = "RIMZ_MESSAGE_SETTLE_MS";
/// Default spacing between discrete message pane writes.
const DEFAULT_MESSAGE_INTERVAL: Duration = Duration::from_secs(1);
const MESSAGE_INTERVAL_ENV: &str = "RIMZ_MESSAGE_INTERVAL_MS";
/// Default gap between raw-typed command segments and before submission,
/// allowing composer paste-burst heuristics to flush.
const DEFAULT_COMMAND_SUBMIT_DELAY: Duration = Duration::from_secs(1);
const COMMAND_SUBMIT_DELAY_ENV: &str = "RIMZ_MESSAGE_COMMAND_SUBMIT_DELAY_MS";
/// Default cap for unconfirmed `Sent` reconciliation attempts.
const DEFAULT_MAX_DELIVERY_ATTEMPTS: u32 = 3;
const MAX_DELIVERY_ATTEMPTS_ENV: &str = "RIMZ_MESSAGE_MAX_DELIVERY_ATTEMPTS";

/// Trigger the card's FIFO head; the detached deliverer rechecks durable state before claiming.
pub fn spawn_queued_head(
    workspace: &crate::ResolvedWorkspace,
    pending: &[MessageRecord],
    card: AgentCardRef<'_>,
    mut spawn: impl FnMut(&RefreshSpawn),
) {
    // A stable name folds provisional and registered ids into the same queue.
    let Some(head) = crate::store::message::queue_head(pending.iter(), card, Timestamp::now())
    else {
        return;
    };
    spawn(&RefreshSpawn {
        args: vec![
            "--root".to_owned(),
            workspace.project_root.display().to_string(),
            "message".to_owned(),
            "deliver".to_owned(),
            "--message-id".to_owned(),
            head.message_id.to_string(),
        ],
    });
}

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
    gate: DeliveryGate,
    sender: MessageSender,
    automated: bool,
    force: bool,
    auto_compact: Option<AutoCompact>,
    not_before: Option<Timestamp>,
    after: Vec<AfterCondition>,
    when: Vec<WhenCondition>,
    reply_wanted: bool,
    premises: Vec<Premise>,
    about_run: Option<crate::ids::RunId>,
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
    launch_id: Option<AgentSessionId>,
    channel: Option<LaneName>,
    pane_id: Option<PaneId>,
}

impl Recipient<'_> {
    fn into_identity(self, scope_channel: Option<&LaneName>) -> RecipientIdentity {
        match self {
            Self::Agent { agent, pane } => RecipientIdentity {
                kind: agent.kind.clone(),
                agent_id: agent.agent_id.clone(),
                agent_name: agent.name.clone(),
                launch_id: agent.launch_id.clone(),
                channel: agent.lane_name().or_else(|| {
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
                launch_id: bound.and_then(|agent| agent.launch_id.clone()),
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
        scope_channel: Option<&LaneName>,
        text: &str,
        address: Option<&str>,
    ) -> MessageRecord {
        let recipient = recipient.into_identity(scope_channel);
        let mut record = MessageRecord::new_for_card(
            workspace_id,
            recipient.kind,
            recipient.agent_id,
            recipient.agent_name,
            text.to_owned(),
            self.gate,
        )
        .with_launch_id(recipient.launch_id)
        .with_body(self.body)
        .with_force(self.force)
        .with_address(address.map(ToOwned::to_owned))
        .with_channel(recipient.channel)
        .with_sender(self.sender.clone())
        .with_automated(self.automated)
        .with_auto_compact(self.auto_compact)
        .with_not_before(self.not_before)
        .with_after(self.after.clone())
        .with_when(self.when.clone())
        .with_about_run(self.about_run.clone())
        .with_premises(self.premises.clone());
        record.reply_wanted = self.reply_wanted;
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
    // Derived statuses (`paused`, `sleeping`) are not raw states.
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

/// Whether boundary delivery to `agent` waits for its resumed provider to report a session. A
/// resume follow-up (`awaits_resume`, a record carrying `register_by`) to a kind whose resume
/// reports its start waits until the provider reports, which the reconciler bounds at that
/// deadline; any other record, and any record to another non-lazy kind, waits one delivery
/// window from the resume stamp.
fn provider_start_pending(agent: &AgentState, awaits_resume: bool, now: Timestamp) -> bool {
    (awaits_resume && agent.resumed_provider_unreported())
        || agent.resumed_at.is_some_and(|resumed_at| {
            now < resumed_at + MessageBody::Prompt.delivery_window()
                && !crate::agents::spec_by_kind(agent.kind.as_str())
                    .is_some_and(|definition| definition.capabilities.registers_lazily)
        })
}

fn gate_open_for_agent(
    gate: DeliveryGate,
    agent: &AgentState,
    force: bool,
    now: Timestamp,
    snapshot: &SidebarSnapshot,
    card_status: &CardStatus,
) -> bool {
    !agent.is_compacting(now)
        && (gate_open(gate, gate_status(gate, agent, now, snapshot, card_status))
            || (force && gate != DeliveryGate::Resume && agent.is_awaiting_input()))
}

fn gate_status(
    gate: DeliveryGate,
    agent: &AgentState,
    now: Timestamp,
    snapshot: &SidebarSnapshot,
    card_status: &CardStatus,
) -> AgentStatus {
    match gate {
        DeliveryGate::Done | DeliveryGate::Any => card_status.status(snapshot, agent, now),
        DeliveryGate::Resume => agent.effective_status(now),
    }
}

pub fn parse_schedule_at(raw: &str, now: &jiff::ZonedDateTime) -> Result<Timestamp, String> {
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
        .to_zoned_datetime(now.time_zone().clone())
        .map_err(|err| format!("schedule `{raw}` cannot be resolved today: {err}"))?;
    if candidate.timestamp() > now.timestamp() {
        return Ok(candidate.timestamp());
    }
    now.date()
        .tomorrow()
        .map_err(|err| format!("schedule `{raw}` cannot be resolved tomorrow: {err}"))?
        .at(time.hour() as i8, time.minute() as i8, 0, 0)
        .to_zoned_datetime(now.time_zone().clone())
        .map(|target| target.timestamp())
        .map_err(|err| format!("schedule `{raw}` cannot be resolved tomorrow: {err}"))
}

pub fn settle_duration_from_env() -> Duration {
    env_ms(SETTLE_ENV).unwrap_or(DEFAULT_SETTLE)
}

/// How long an interrupt waits after its key for the receiver's row to show the stop.
fn interrupt_confirmation_window_from_env() -> Duration {
    env_ms("RIMZ_MESSAGE_INTERRUPT_CONFIRM_MS").unwrap_or(Duration::from_secs(5))
}

/// Spacing between discrete message pane writes.
fn message_interval_from_env() -> Duration {
    env_ms(MESSAGE_INTERVAL_ENV).unwrap_or(DEFAULT_MESSAGE_INTERVAL)
}

/// Gap between raw-typed command text and its submit keystroke.
fn command_submit_delay_from_env() -> Duration {
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
