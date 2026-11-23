//! `message`: the job of `rimz message <address> <text>`, through the one dispatch the command
//! goes through, and on an address that names no live agent the resume of the caller's ended
//! child the command makes.

use serde::Serialize;

use super::Tools;
use super::notices::CollectedNotices;
use super::room::Room;
use crate::address::{LaneSelector, TargetErr};
use crate::launcher::child_resume::{ResumeChildOutcome, resume_child};
use crate::launcher::context::{current_channel, resolve_caller, resolve_me, sender_for};
use crate::mcp::catalog::{MessageArgs, MessageMode};
use crate::mcp::{Answer, Failure};
use crate::message::dispatch::{
    DispatchErr, DispatchMode, DispatchOutcome, DispatchRequest, DispatchResult, ParkReason,
};
use crate::message::{InputWait, InterruptConfirmation};
use crate::store::message::{DeliveryGate, MessageSender, Premise};

#[derive(Serialize)]
struct Sent {
    messages: Vec<Row>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    compacted: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

#[derive(Serialize)]
struct Row {
    to: String,
    message_id: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

/// Send `args.text` as the caller, from the room the server was started for.
pub(super) fn send(tools: &Tools, args: &MessageArgs) -> Result<Answer, Failure> {
    let room = Room::open(&tools.dir(None)?)?;
    let (workspace, store) = (&room.workspace, &room.store);
    crate::address::require_mention(&args.to).map_err(|err| Failure::new(err.to_string()))?;
    let to = match args.to.as_str() {
        "@me" => {
            let snapshot = store
                .snapshot_cached()
                .map_err(|err| Failure::from(anyhow::Error::new(err)))?;
            format!("@{}", resolve_me(store, &snapshot)?.agent_id)
        }
        to => to.to_owned(),
    };
    let caller = resolve_caller(store)?;
    let channel = current_channel(workspace, Some(store));
    let sender = sender_for(caller.as_ref(), channel.as_deref(), false);
    let lane = channel.address_context().lane();
    if args.reply_wanted && !matches!(sender, MessageSender::Agent { .. }) {
        return Err(Failure::new(
            "`reply_wanted` asks for a message back, and only an agent RimZ can identify has an inbox one could reach",
        )
        .with_hint("send without `reply_wanted`"));
    }
    let request = || DispatchRequest {
        target: to.clone(),
        text: args.text.trim().to_owned(),
        target_scope: None,
        current_channel: channel.address_context(),
        caller: caller.clone(),
        sender: sender.clone(),
        automated: false,
        allow_fanout: args.all,
        mux: None,
        force: false,
        auto_compact: None,
        mode: match args.mode {
            MessageMode::Park => DispatchMode::Boundary {
                gate: DeliveryGate::Done,
                not_before: None,
                after: Vec::new(),
                when: Vec::new(),
            },
            MessageMode::Steer => DispatchMode::Steer,
            MessageMode::Interrupt => DispatchMode::Interrupt,
        },
        resuming_recipient: None,
        reply_wanted: args.reply_wanted,
        premises: Premise::NONE,
        about_run: None,
        selected: Vec::new(),
    };
    let mut notices = CollectedNotices::default();
    let result = match crate::message::dispatch::dispatch(workspace, store, request()) {
        Ok(result) => result,
        Err(DispatchErr::Recipient(
            err @ (TargetErr::NoMatch { .. }
            | TargetErr::NoMatchInChannel { .. }
            | TargetErr::PaneUnbound { .. }),
        )) => {
            let resumed = resume_child(
                workspace,
                store,
                &to,
                None,
                &channel.address_context(),
                request(),
                &mut notices,
            );
            match resumed {
                Ok(ResumeChildOutcome::Dispatched(result)) => result,
                Ok(ResumeChildOutcome::AlreadyBound) => {
                    match crate::message::dispatch::dispatch(workspace, store, request()) {
                        Ok(result) => result,
                        Err(err @ DispatchErr::Recipient(_)) => {
                            record_bounce(
                                &room,
                                args,
                                &sender,
                                channel.as_deref(),
                                "receiver not found",
                            )?;
                            return Err(dispatch_failure(&room, &lane, err));
                        }
                        Err(err) => return Err(dispatch_failure(&room, &lane, err)),
                    }
                }
                Ok(ResumeChildOutcome::DispatchFailed(err)) => {
                    return Err(dispatch_failure(&room, &lane, err));
                }
                Ok(ResumeChildOutcome::DeliveryFailed(err)) => return Err(err.into()),
                Ok(ResumeChildOutcome::LimitRefusal(reason)) => {
                    record_bounce(&room, args, &sender, channel.as_deref(), &reason)?;
                    return Err(Failure::new(reason));
                }
                Ok(ResumeChildOutcome::NotApplicable) => {
                    record_bounce(
                        &room,
                        args,
                        &sender,
                        channel.as_deref(),
                        "receiver not found",
                    )?;
                    return Err(no_receiver(&room, &lane, &err));
                }
                Err(resume) => {
                    let reason = format!("{resume:#}");
                    record_bounce(&room, args, &sender, channel.as_deref(), &reason)?;
                    return Err(Failure::new(format!("{reason}: {err}")));
                }
            }
        }
        Err(err) => return Err(dispatch_failure(&room, &lane, err)),
    };
    Ok(Answer::data(&answer(result, notices.into_lines())))
}

/// The dispatch's outcomes as rows, each interrupt the receiver did not confirm a warning.
fn answer(result: DispatchResult, mut warnings: Vec<String>) -> Sent {
    let messages = result
        .outcomes
        .into_iter()
        .map(|outcome| match outcome {
            DispatchOutcome::Sent {
                label,
                message_id,
                interrupt,
                ..
            } => {
                warnings.extend(interrupt_warning(&label, interrupt));
                row(label, &message_id, "sent", None)
            }
            DispatchOutcome::Queued {
                label,
                message_id,
                reason,
                ..
            } => {
                let detail = reason
                    .as_ref()
                    .map(|reason| park_detail(&label, reason, &message_id));
                row(label, &message_id, "queued", detail)
            }
            DispatchOutcome::CompactionPending {
                label, message_id, ..
            } => row(
                label,
                &message_id,
                "queued",
                Some("delivers when its compaction completes".to_owned()),
            ),
            DispatchOutcome::SkippedWaiting {
                label,
                message_id,
                reason,
                ..
            } => {
                let detail = input_wait(reason).to_owned();
                row(label, &message_id, "not_sent", Some(detail))
            }
        })
        .collect();
    Sent {
        messages,
        compacted: result.compacted,
        warnings,
    }
}

fn row(
    to: String,
    message_id: &crate::ids::MessageId,
    status: &'static str,
    detail: Option<String>,
) -> Row {
    Row {
        to,
        message_id: message_id.to_string(),
        status,
        detail,
    }
}

/// Why a queued message waits, where that is more than the receiver's next turn boundary.
fn park_detail(label: &str, reason: &ParkReason, message_id: &crate::ids::MessageId) -> String {
    match reason {
        ParkReason::TypingHeld { until } => format!(
            "someone is using {label}'s pane; it is tried again after {}",
            crate::utils::time::format_local_timestamp(*until)
        ),
        ParkReason::Status(status) => format!("{label} is {}", status.as_str()),
        ParkReason::WaitingOnPrompt => {
            format!("{label} has a prompt open and reads nothing until the user answers it")
        }
        ParkReason::PaneHeld => format!(
            "{label} ended its turn on a provider error; its pane is held until its next activity"
        ),
        ParkReason::ModelHeld(hold) => hold.to_string(),
        ParkReason::ProviderStarting => {
            format!("{label} is resuming; it delivers when its provider registers")
        }
        ParkReason::Resuming { register_by, .. } => format!(
            "{label} is resuming; it delivers once it is up, and fails if it is not by {}",
            crate::utils::time::format_local_timestamp(*register_by)
        ),
        ParkReason::Scheduled(at) => format!(
            "opens at {}",
            crate::utils::time::format_local_timestamp(*at)
        ),
        ParkReason::After(address) => format!("after {address}"),
        ParkReason::When {
            address,
            status,
            dwell_secs,
        } => format!(
            "when {address} {} {}",
            status.as_str(),
            crate::store::message::format_dwell(*dwell_secs)
        ),
        ParkReason::Behind(blocker) => format!("behind {blocker}"),
        ParkReason::NoPane => format!("{label} has no live pane"),
        ParkReason::NotHosted { blocker } => blocker
            .strip_prefix("stuck: ")
            .unwrap_or(blocker)
            .to_owned(),
        ParkReason::InterruptWithheld { cause, pressed } => {
            crate::message::interrupt_withheld_detail(cause, *pressed, message_id)
        }
    }
}

/// Why a steer or an interrupt left the text untyped, without the command's `--force`, which the
/// tool does not take.
fn input_wait(reason: InputWait) -> &'static str {
    match reason {
        InputWait::Human => "it is waiting on the user's input in its pane",
        InputWait::PermissionReview => {
            "its permission request is under the provider's automatic review; try again shortly"
        }
        InputWait::TurnEnded => {
            "it ended its turn on a provider error, and an approval may still be open in its pane"
        }
    }
}

fn interrupt_warning(label: &str, confirmation: InterruptConfirmation) -> Option<String> {
    confirmation
        .warning(label)
        .map(|warning| format!("{label}: {warning}"))
}

/// A send that matched no agent, recorded as the command records it, so the room's account of
/// bounced messages holds both.
fn record_bounce(
    room: &Room,
    args: &MessageArgs,
    sender: &MessageSender,
    channel: Option<&crate::ids::LaneName>,
    reason: &str,
) -> Result<(), Failure> {
    let trunk = crate::ids::LaneName::trunk();
    room.store
        .record_unresolved_message(crate::store::writer::UnresolvedMessage {
            workspace_id: room.workspace.workspace_id.clone(),
            session_name: &room.workspace.session_name,
            address: &args.to,
            channel: channel.or(Some(&trunk)),
            sender,
            text_len: args.text.trim().len(),
            reason,
        })
        .map_err(|err| Failure::from(anyhow::Error::new(err)))
}

/// The most live agents a refusal names; a room can hold hundreds, and the rest are a
/// `rimz agents list` away.
const LISTED_AGENTS: usize = 20;

/// An address that named no agent, with the live agents in the caller's lane to pick from: a
/// bare handle resolves only there.
fn no_receiver(room: &Room, lane: &Option<LaneSelector>, err: &TargetErr) -> Failure {
    let failure = Failure::new(err.to_string());
    let Ok(snapshot) = room.store.snapshot_cached() else {
        return failure;
    };
    let live: Vec<String> = crate::address::addressable_agents(&snapshot)
        .into_iter()
        .filter(|agent| {
            lane.as_ref()
                .is_none_or(|lane| crate::address::agent_in_lane(agent, lane))
        })
        .filter_map(|agent| agent.name.as_ref().map(|name| format!("@{name}")))
        .collect();
    if live.is_empty() {
        return failure.with_hint("no agents are running");
    }
    let listed = live[..live.len().min(LISTED_AGENTS)].join(", ");
    match live
        .len()
        .checked_sub(LISTED_AGENTS)
        .filter(|more| *more > 0)
    {
        Some(more) => failure.with_hint(format!(
            "live agents: {listed}, and {more} more that `rimz agents list` shows"
        )),
        None => failure.with_hint(format!("live agents: {listed}")),
    }
}

fn dispatch_failure(room: &Room, lane: &Option<LaneSelector>, err: DispatchErr) -> Failure {
    match err {
        DispatchErr::Fanout {
            target, matches, ..
        } => Failure::new(format!(
            "`{target}` matches {} agents: {}",
            matches.len(),
            matches
                .iter()
                .map(|candidate| candidate.address.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .with_hint("name one of them in `to`, or set `all` to send to each"),
        DispatchErr::Recipient(err) => no_receiver(room, lane, &err),
        err => Failure::from(anyhow::Error::new(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_withheld_interrupt_says_whether_its_key_went() {
        let id = crate::ids::MessageId::new();
        let detail = |pressed| {
            let reason = ParkReason::InterruptWithheld {
                cause: "the receiver left its pane",
                pressed,
            };
            park_detail("@coder", &reason, &id)
        };
        assert_eq!(
            detail(false),
            format!(
                "not interrupted: the receiver left its pane; send now: rimz message interrupt {id}"
            )
        );
        assert_eq!(
            detail(true),
            format!(
                "interrupted, text not sent: the receiver left its pane; send it: rimz message steer {id}"
            )
        );
    }
}
