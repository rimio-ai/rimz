use super::*;

#[derive(Clone, Debug, Serialize)]
pub(super) struct MessageTimelineRow {
    method: String,
    at: Timestamp,
    attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct MessageDeliveryJson {
    check: deliver::DeliveryCheck,
    verdict: String,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct MessageShowJson {
    message: MessageListRow,
    timeline: Vec<MessageTimelineRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery: Option<MessageDeliveryJson>,
}

pub(super) fn show_message(message_id: MessageId, json: bool, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let store = &ctx.store;
    let cached_snapshot = ctx.cached_snapshot()?;
    let Some(message) = projected_messages(store)?
        .into_iter()
        .find(|message| message.message_id == message_id)
    else {
        bail!("message {message_id} not found");
    };
    let timeline = message_timeline(store, &message_id)?;
    let live_messages = store.list_messages()?;
    let now = Timestamp::now();
    let delivery = open_delivery(&ctx, &message, &live_messages, now)?;
    if json {
        return render::json_pretty(&MessageShowJson {
            message,
            timeline,
            delivery,
        });
    }

    let agents = rimz::address::addressable_agents(&cached_snapshot);
    let root_channel = rimz::address::root_lane_channel(&ctx.workspace.project_root);
    let lane_label =
        rimz::address::record_lane_label(message.channel.as_deref(), root_channel.as_deref());
    let raw_target = message_target(&message, &agents, root_channel.as_deref());
    let target = scoped_handle(raw_target.clone(), lane_label);
    let sender = scoped_handle(
        message_sender(&message.sender, &agents, root_channel.as_deref()),
        lane_label,
    );
    let prose = render::prose::Prose::for_stdout();
    let mut out = render::out();
    writeln!(
        out,
        "{} — {}",
        render::paint(
            render::palette::accent().bold(),
            message.message_id.as_str()
        ),
        render::paint(
            render::status::message(message.status),
            message.status.as_str()
        )
    )?;
    let kv = render_message_kv(&message, &target, &sender, lane_label, now);
    kv.render(&mut out)?;
    writeln!(out)?;
    writeln!(out, "{}", render::paint(render::palette::header(), "TEXT"))?;
    if let Some(text) = message.text.as_deref() {
        write_indented_block(&mut out, text, prose)?;
    } else {
        writeln!(out, "  ({})", textless_location(&message, &raw_target))?;
    }
    writeln!(out)?;
    render_timeline(&mut out, &timeline, now)?;
    if let Some(delivery) = delivery {
        render_delivery_check(
            &mut out,
            &message.message_id,
            &delivery.check,
            &delivery.verdict,
            &raw_target,
            now,
        )?;
    }
    Ok(())
}

fn open_delivery(
    ctx: &Ctx,
    message: &MessageListRow,
    live_messages: &[MessageRecord],
    now: Timestamp,
) -> Result<Option<MessageDeliveryJson>> {
    if !message.status.is_open() {
        return Ok(None);
    }
    let Some(record) = live_messages
        .iter()
        .find(|record| record.message_id == message.message_id)
    else {
        return Ok(None);
    };
    let snapshot = ctx.fold_agent_context(ctx.resolution_snapshot()?);
    let mut check = deliver::explain(record, live_messages, &snapshot, now);
    let agents = rimz::address::addressable_agents(&snapshot);
    let root_channel = rimz::address::root_lane_channel(&ctx.workspace.project_root);
    let target = message_target(message, &agents, root_channel.as_deref());
    let verdict = check.verdict();
    let audit_receiver = if verdict == deliver::DeliveryVerdict::ReceiverEnded {
        snapshot
            .agents
            .iter()
            .find(|agent| record.same_agent_card(agent))
            .cloned()
    } else if verdict == deliver::DeliveryVerdict::ReceiverGone {
        ctx.store
            .runtime_projection(rimz::RuntimeScope::Audit)
            .ok()
            .and_then(|projection| {
                projection
                    .agents
                    .into_iter()
                    .find(|agent| record.same_agent_card(agent))
            })
    } else {
        None
    };
    if audit_receiver
        .as_ref()
        .is_some_and(|receiver| receiver.ended_at.is_some())
    {
        check.agent.present = true;
        check.agent.ended = true;
    }
    Ok(Some(MessageDeliveryJson {
        verdict: render_verdict(&check.verdict(), &target, audit_receiver.as_ref(), now),
        check,
    }))
}

fn render_message_kv(
    message: &MessageListRow,
    target: &str,
    sender: &str,
    lane_label: Option<&str>,
    now: Timestamp,
) -> render::KeyVals {
    let mut kv = render::KeyVals::new().indent(2);
    kv.push("from", render::cell(sender).fg(render::palette::meta()));
    kv.push("to", render::cell(target).fg(render::palette::meta()));
    kv.push("channel", render::cell(lane_label.unwrap_or("-")).dash());
    if message.body != MessageBody::Prompt {
        kv.push("body", render::cell(message.body.as_str()));
    }
    if message.gate != DeliveryGate::Done {
        kv.push("gate", render::cell(message.gate.as_str()));
    }
    if !message.enter {
        kv.push("enter", render::cell("false"));
    }
    if message.force {
        kv.push("force", render::cell("true"));
    }
    kv.push(
        "created",
        render::cell(time_with_absolute(message.enqueued_at, now)),
    );
    if let Some(delivered) = message.delivered_at {
        kv.push(
            "delivered",
            render::cell(time_with_absolute(delivered, now)),
        );
    }
    if let Some(not_before) = message.not_before {
        kv.push(
            "schedule",
            render::cell(time_until_with_absolute(not_before, now)),
        );
    }
    if !message.after.is_empty() {
        kv.push(
            "after",
            render::cell(
                message
                    .after
                    .iter()
                    .map(|condition| match condition.met_at {
                        Some(_) => format!("{} ✓", condition.address),
                        None => format!("{} waiting", condition.address),
                    })
                    .collect::<Vec<_>>()
                    .join(" · "),
            ),
        );
    }
    if !message.when.is_empty() {
        kv.push(
            "when",
            render::cell(
                message
                    .when
                    .iter()
                    .map(|condition| {
                        let met = condition.met_at.is_some();
                        let label = format!(
                            "{} {} {}",
                            condition.address,
                            condition.status.as_str(),
                            rimz::store::message::format_dwell(condition.dwell_secs)
                        );
                        if met {
                            format!("{label} ✓")
                        } else {
                            format!("{label} waiting")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" · "),
            ),
        );
    }
    if message.attempts > 0 {
        kv.push("attempts", render::cell(message.attempts.to_string()));
    }
    if message.unconfirmed_sends > 0 {
        kv.push(
            "unconfirmed_sends",
            render::cell(message.unconfirmed_sends.to_string()),
        );
    }
    if let Some(last_error) = message.last_error.as_deref() {
        kv.push("last_error", render::cell(last_error));
    }
    kv
}

fn render_timeline(
    out: &mut impl Write,
    timeline: &[MessageTimelineRow],
    now: Timestamp,
) -> Result<()> {
    writeln!(
        out,
        "{}",
        render::paint(render::palette::header(), "TIMELINE")
    )?;
    if timeline.is_empty() {
        writeln!(out, "  -")?;
        return Ok(());
    }
    let show_attempts = timeline.iter().any(|event| event.attempts > 0);
    let show_note = timeline.iter().any(|event| {
        event
            .reason
            .as_deref()
            .is_some_and(|reason| !reason.is_empty())
    });
    let mut headers = vec!["EVENT", "WHEN"];
    if show_attempts {
        headers.push("ATTEMPT");
    }
    if show_note {
        headers.push("NOTE");
    }
    let mut table = render::Table::new(headers).indent(2);
    for event in timeline {
        let label = event
            .method
            .strip_prefix("message.")
            .unwrap_or(&event.method);
        let mut row = vec![
            render::cell(label.to_owned()),
            render::cell(time_with_absolute(event.at, now)),
        ];
        if show_attempts {
            row.push(render::cell(event.attempts.to_string()));
        }
        if show_note {
            let reason = event
                .reason
                .as_deref()
                .filter(|reason| !reason.is_empty())
                .unwrap_or("-");
            row.push(render::cell(reason).dash());
        }
        table.row(row);
    }
    table.render(out)?;
    Ok(())
}

pub(super) fn message_timeline(
    store: &rimz::Store,
    message_id: &MessageId,
) -> Result<Vec<MessageTimelineRow>> {
    let mut rows = Vec::new();
    for event in store.read_events()? {
        let EventKind::Message { method, payload } = event.kind() else {
            continue;
        };
        if payload.message_id != *message_id {
            continue;
        }
        rows.push(MessageTimelineRow {
            method: method.as_str().to_owned(),
            at: event.timestamp,
            attempts: payload.attempts,
            reason: payload.reason,
        });
    }
    Ok(rows)
}

pub(super) fn steer_failure(
    check: &deliver::DeliveryCheck,
    target: &str,
    message_id: &MessageId,
) -> String {
    let verdict = check.verdict();
    if matches!(verdict, deliver::DeliveryVerdict::Expired { .. }) {
        return render_verdict(&verdict, target, None, Timestamp::now());
    }
    if check.ask.waiting {
        return format!(
            "{target} ({message_id}) is waiting on your input in its pane; answer it or pass --force"
        );
    }
    if !check.agent.present {
        return format!("receiver {target} is gone; cannot steer {message_id}");
    }
    if !check.pane.present {
        return match &check.pane.pinned_pane_id {
            Some(pane_id) => {
                format!("pinned pane {pane_id} is not live for {target}; cannot steer {message_id}")
            }
            None => format!("no live pane for {target}; cannot steer {message_id}"),
        };
    }
    if check.passes() {
        return format!(
            "{message_id} has a recent delivery attempt in progress; retry in a few seconds"
        );
    }
    render_verdict(&verdict, target, None, Timestamp::now())
}

pub(super) fn time_with_absolute(ts: Timestamp, now: Timestamp) -> String {
    let absolute = ts.strftime("%Y-%m-%dT%H:%M:%SZ");
    format!("{} ({absolute})", render::rel_age(ts, now))
}

pub(super) fn time_until_with_absolute(ts: Timestamp, now: Timestamp) -> String {
    let absolute = ts.strftime("%Y-%m-%dT%H:%M:%SZ");
    format!("{} ({absolute})", render::rel_until(ts, now))
}

pub(super) fn write_indented_block(
    out: &mut impl Write,
    text: &str,
    prose: render::prose::Prose,
) -> Result<()> {
    if text.is_empty() {
        writeln!(out, "  ")?;
        return Ok(());
    }
    if prose == render::prose::Prose::Raw {
        for line in text.split('\n') {
            writeln!(out, "  {line}")?;
        }
        return Ok(());
    }
    for line in prose.lines(text, render::prose::prose_width(2)) {
        writeln!(out, "  {line}")?;
    }
    Ok(())
}

pub(super) fn textless_location(message: &MessageListRow, target: &str) -> String {
    if let Some(reason) = message
        .last_error
        .as_deref()
        .filter(|reason| !reason.is_empty())
    {
        return format!("content not retained in the event log; {reason}");
    }
    if message.status == MessageStatus::Delivered {
        format!("content in `rimz transcript {target}`")
    } else {
        "content not retained in the event log".to_owned()
    }
}

pub(super) fn render_delivery_check(
    out: &mut impl Write,
    message_id: &MessageId,
    check: &deliver::DeliveryCheck,
    verdict: &str,
    target: &str,
    now: Timestamp,
) -> Result<()> {
    writeln!(out)?;
    writeln!(
        out,
        "{}",
        render::paint(render::palette::header(), "DELIVERY CHECK")
    )?;
    let mut kv = render::KeyVals::new().indent(2);
    let (ok, detail) = schedule_detail(check, now);
    kv.push("schedule", condition_cell(ok, detail));
    if !check.after.is_empty() {
        let (ok, detail) = after_detail(check);
        kv.push("after", condition_cell(ok, detail));
    }
    if !check.when.is_empty() {
        let (ok, detail) = when_detail(check, now);
        kv.push("when", condition_cell(ok, detail));
    }
    let (ok, detail) = fifo_detail(check);
    kv.push("fifo", condition_cell(ok, detail));
    kv.push(
        "agent",
        condition_cell(
            check.agent.present && !check.agent.ended,
            if check.agent.ended {
                "receiver ended".to_owned()
            } else if check.agent.present {
                "ok".to_owned()
            } else {
                "receiver gone".to_owned()
            },
        ),
    );
    let (ok, detail) = gate_detail(check);
    kv.push("gate", condition_cell(ok, detail));
    let (ok, detail) = ask_detail(check);
    kv.push("ask", condition_cell(ok, detail));
    let (ok, detail) = pane_detail(check);
    kv.push("pane", condition_cell(ok, detail));
    kv.render(out)?;
    writeln!(out, "  {verdict}")?;
    if let Some(hint) = delivery_action_hint(&check.verdict(), message_id, target) {
        writeln!(out, "  {}", render::paint(render::palette::faint(), &hint))?;
    }
    Ok(())
}

fn schedule_detail(check: &deliver::DeliveryCheck, now: Timestamp) -> (bool, String) {
    let detail = if check.schedule.ready {
        match check.schedule.retry_after {
            Some(retry_after) if retry_after > now => format!(
                "ok; retry wait {}",
                time_until_with_absolute(retry_after, now)
            ),
            Some(retry_after) => format!("ok; retry wait {}", time_with_absolute(retry_after, now)),
            None => "ok".to_owned(),
        }
    } else {
        check
            .schedule
            .not_before
            .map(|not_before| format!("opens {}", time_until_with_absolute(not_before, now)))
            .unwrap_or_else(|| "not ready".to_owned())
    };
    (check.schedule.ready, detail)
}

fn after_detail(check: &deliver::DeliveryCheck) -> (bool, String) {
    let ready = check.after.iter().all(|condition| condition.met);
    let detail = check
        .after
        .iter()
        .map(|condition| {
            if condition.met {
                format!("{} ok", condition.address)
            } else if condition.agent_present {
                format!("{} waiting", condition.address)
            } else {
                format!("{} not running", condition.address)
            }
        })
        .collect::<Vec<_>>()
        .join(" · ");
    (ready, detail)
}

fn when_detail(check: &deliver::DeliveryCheck, now: Timestamp) -> (bool, String) {
    let ready = check.when.iter().all(|condition| condition.met);
    let detail = check
        .when
        .iter()
        .map(|condition| {
            let wanted = format!(
                "{} {} {}",
                condition.address,
                condition.expected.as_str(),
                rimz::store::message::format_dwell(condition.dwell_secs)
            );
            if condition.met {
                format!("{wanted} ok")
            } else if let Some(elapsed) = condition.dwell_so_far_secs {
                let progress = format!(
                    "{wanted}; {} so far",
                    rimz::store::message::format_dwell(elapsed)
                );
                condition.trip_at.map_or(progress.clone(), |trip_at| {
                    format!(
                        "{progress}; trips {}",
                        time_until_with_absolute(trip_at, now)
                    )
                })
            } else if let Some(status) = condition.status {
                format!("{wanted}; currently {}", status.as_str())
            } else {
                format!("{wanted}; agent ended")
            }
        })
        .collect::<Vec<_>>()
        .join(" · ");
    (ready, detail)
}

fn fifo_detail(check: &deliver::DeliveryCheck) -> (bool, String) {
    let detail = if check.fifo.head {
        "ok".to_owned()
    } else {
        check
            .fifo
            .blocker
            .as_ref()
            .map(|blocker| format!("behind {blocker}"))
            .unwrap_or_else(|| "head unavailable".to_owned())
    };
    (check.fifo.head, detail)
}

fn gate_detail(check: &deliver::DeliveryCheck) -> (bool, String) {
    let detail = if check.gate.open && check.gate.provider_start_pending {
        "waiting for provider registration".to_owned()
    } else if check.gate.resume_recovered == Some(false) {
        "waiting for provider recovery".to_owned()
    } else if check.gate.open {
        match check.gate.status {
            Some(status) => format!("ok (status {})", status.as_str()),
            None => "ok".to_owned(),
        }
    } else {
        match check.gate.status {
            Some(status) => format!(
                "closed (status {}, gate {})",
                status.as_str(),
                check.gate.gate
            ),
            None => format!("closed (gate {})", check.gate.gate),
        }
    };
    (check.gate_ready(), detail)
}

fn ask_detail(check: &deliver::DeliveryCheck) -> (bool, String) {
    let detail = if check.ask.waiting {
        "waiting in pane".to_owned()
    } else if check.ask.force {
        "ok (forced)".to_owned()
    } else {
        "ok".to_owned()
    };
    (!check.ask.waiting, detail)
}

fn pane_detail(check: &deliver::DeliveryCheck) -> (bool, String) {
    let detail = if check.pane.present {
        check
            .pane
            .pane_id
            .as_ref()
            .map(|pane_id| format!("ok ({pane_id})"))
            .unwrap_or_else(|| "ok".to_owned())
    } else if let Some(pane_id) = &check.pane.pinned_pane_id {
        format!("pinned pane {pane_id} not live")
    } else {
        "no live pane".to_owned()
    };
    (check.pane.present, detail)
}

pub(super) fn condition_cell(ok: bool, text: String) -> render::Cell {
    let style = if ok {
        render::palette::good()
    } else {
        render::palette::warn()
    };
    render::cell(text).fg(style)
}

pub(super) fn render_verdict(
    verdict: &deliver::DeliveryVerdict,
    target: &str,
    audit_receiver: Option<&AgentState>,
    now: Timestamp,
) -> String {
    match verdict {
        deliver::DeliveryVerdict::Expired { expires_at } => {
            let validity = rimz::store::message::command_validity();
            let queued_at = *expires_at - validity;
            let age = now.duration_since(queued_at).as_secs().max(0) as u64;
            format!(
                "expired: automatic command valid for {}, queued {} ago; the next sweep records it expired",
                rimz::store::message::format_dwell(validity.as_secs()),
                rimz::store::message::format_dwell(age),
            )
        }
        deliver::DeliveryVerdict::Scheduled { not_before } => not_before
            .as_ref()
            .copied()
            .map(|not_before| {
                format!(
                    "scheduled: opens {}",
                    time_until_with_absolute(not_before, now)
                )
            })
            .unwrap_or_else(|| "scheduled: waiting for readiness floor".to_owned()),
        deliver::DeliveryVerdict::WaitingOnAfter {
            address,
            agent_present,
        } => {
            if *agent_present {
                format!("waiting on {address} to finish")
            } else {
                format!("waiting on {address} to finish ({address} not running)")
            }
        }
        deliver::DeliveryVerdict::WaitingOnWhen {
            address,
            expected,
            current,
            dwell_secs,
            dwell_so_far_secs,
        } => {
            let dwell = rimz::store::message::format_dwell(*dwell_secs);
            if let Some(elapsed) = dwell_so_far_secs {
                format!(
                    "waiting for {address} {} ≥ {dwell} — {} {} so far",
                    expected.as_str(),
                    expected.as_str(),
                    rimz::store::message::format_dwell(*elapsed)
                )
            } else {
                let current = current
                    .map(|status| status.as_str())
                    .unwrap_or("not running");
                format!(
                    "waiting for {address} to be {} (currently {current})",
                    expected.as_str()
                )
            }
        }
        deliver::DeliveryVerdict::BehindFifo { blocker } => blocker
            .as_ref()
            .map(|blocker| format!("blocked: behind {blocker}"))
            .unwrap_or_else(|| "blocked: FIFO head unavailable".to_owned()),
        deliver::DeliveryVerdict::ReceiverGone => audit_receiver.map_or_else(
            || format!("stuck: receiver {target} is gone"),
            |receiver| {
                format!(
                    "stuck: receiver {target} is gone; its session record survives (last seen {}), but no live process claims it; try `rimz agents resume`",
                    time_with_absolute(receiver.last_seen, now)
                )
            },
        ),
        deliver::DeliveryVerdict::ReceiverEnded => {
            let mut sentence = format!("stuck: receiver {target} has ended");
            if audit_receiver.is_some_and(AgentState::is_launched_child) {
                sentence.push_str(&format!("; rimz message {target} resumes it"));
            }
            sentence
        }
        deliver::DeliveryVerdict::Compacting => {
            format!("waiting: {target} is compacting its context")
        }
        deliver::DeliveryVerdict::GateClosed { gate, status } => {
            let status = status
                .as_ref()
                .copied()
                .map(|status| status.as_str())
                .unwrap_or("unknown");
            if *gate == DeliveryGate::Resume {
                format!(
                    "waiting: {target} is {status}; resume gate opens when the agent is paused and provider recovery passes"
                )
            } else {
                format!("waiting: {target} is {status}; gate '{gate}' opens at next turn end")
            }
        }
        deliver::DeliveryVerdict::ResumeUnrecovered => {
            format!("waiting: {target} is paused; resume gate opens after provider recovery")
        }
        deliver::DeliveryVerdict::ProviderStarting => {
            format!("waiting: {target} is resuming; delivers when its provider registers")
        }
        deliver::DeliveryVerdict::AskWaiting => {
            format!("waiting: {target} is waiting on input in its pane")
        }
        deliver::DeliveryVerdict::NoPane { pinned_pane_id } => {
            format!("{} for {target}", deliver::no_pane_blocker(pinned_pane_id.as_ref()))
        }
        deliver::DeliveryVerdict::Ready => "ready: delivery conditions pass".to_owned(),
    }
}

pub(super) fn delivery_action_hint(
    verdict: &deliver::DeliveryVerdict,
    message_id: &MessageId,
    target: &str,
) -> Option<String> {
    match verdict {
        deliver::DeliveryVerdict::Scheduled { .. } => Some(format!(
            "force now: rimz message steer {message_id}  ·  or: rimz message edit {message_id} --no-schedule"
        )),
        deliver::DeliveryVerdict::WaitingOnAfter { .. }
        | deliver::DeliveryVerdict::WaitingOnWhen { .. }
        | deliver::DeliveryVerdict::BehindFifo { .. }
        | deliver::DeliveryVerdict::ReceiverGone
        | deliver::DeliveryVerdict::Compacting
        | deliver::DeliveryVerdict::GateClosed { .. }
        | deliver::DeliveryVerdict::ProviderStarting
        | deliver::DeliveryVerdict::ResumeUnrecovered => {
            Some(format!("force now: rimz message steer {message_id}"))
        }
        deliver::DeliveryVerdict::AskWaiting => Some(format!(
            "see it: rimz asks show {target}      answer it: rimz answer {target} <choice>"
        )),
        deliver::DeliveryVerdict::NoPane { .. } | deliver::DeliveryVerdict::Ready => None,
        deliver::DeliveryVerdict::ReceiverEnded => None,
        deliver::DeliveryVerdict::Expired { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_wait_hint_routes_to_the_prompt_and_answer() {
        let id = "msg_0000000000000001".parse().unwrap();
        assert_eq!(
            delivery_action_hint(&deliver::DeliveryVerdict::AskWaiting, &id, "@coder").as_deref(),
            Some("see it: rimz asks show @coder      answer it: rimz answer @coder <choice>")
        );
    }

    use rimz::agents::AgentStatus;
    use rimz::ids::{MessageId, MuxName, PaneId};

    #[test]
    fn ended_verdict_offers_only_a_launched_child_the_parent_message_retry() {
        let verdict = deliver::DeliveryVerdict::ReceiverEnded;
        let id = MessageId::parse("msg_0000000000000001").unwrap();
        let mut receiver = rimz::testkit::agent_state("claude", "child", Timestamp::UNIX_EPOCH);
        receiver.ended_at = Some(Timestamp::UNIX_EPOCH);
        assert_eq!(
            render_verdict(&verdict, "@otter", Some(&receiver), Timestamp::UNIX_EPOCH),
            "stuck: receiver @otter has ended"
        );
        receiver.parent_agent_id = Some("parent".into());
        receiver.launch_depth = Some(1);
        assert_eq!(
            render_verdict(&verdict, "@otter", Some(&receiver), Timestamp::UNIX_EPOCH),
            "stuck: receiver @otter has ended; rimz message @otter resumes it"
        );
        assert_eq!(delivery_action_hint(&verdict, &id, "@otter"), None);
    }

    #[test]
    fn resuming_verdict_names_registration_and_steer() {
        let verdict = deliver::DeliveryVerdict::ProviderStarting;
        let id = MessageId::parse("msg_0000000000000001").unwrap();
        assert_eq!(
            render_verdict(&verdict, "@otter", None, Timestamp::UNIX_EPOCH),
            "waiting: @otter is resuming; delivers when its provider registers"
        );
        assert_eq!(
            delivery_action_hint(&verdict, &id, "@otter"),
            Some(format!("force now: rimz message steer {id}"))
        );
    }

    #[test]
    fn delivery_checks_report_recent_attempt_and_after_blocker() {
        let message_id = MessageId::parse("msg_0000000000000001").unwrap();
        let mut check = deliver::DeliveryCheck {
            expiry: deliver::ExpiryCheck {
                expired: false,
                expires_at: None,
            },
            schedule: deliver::ScheduleCheck {
                ready: true,
                not_before: None,
                retry_after: None,
            },
            after: Vec::new(),
            when: Vec::new(),
            fifo: deliver::FifoCheck {
                head: true,
                blocker: None,
            },
            agent: deliver::AgentCheck {
                present: true,
                ended: false,
            },
            gate: deliver::GateCheck {
                provider_start_pending: false,
                gate: DeliveryGate::Done,
                status: Some(AgentStatus::Idle),
                compacting: false,
                open: true,
                resume_recovered: None,
            },
            ask: deliver::AskCheck {
                waiting: false,
                force: false,
            },
            pane: deliver::PaneCheck {
                present: true,
                pane_id: Some(PaneId::from_parts(MuxName::Zellij, "terminal_3")),
                pinned_pane_id: None,
            },
        };

        let message = steer_failure(&check, "@claude", &message_id);

        assert!(message.contains("recent delivery attempt"), "{message}");
        assert!(!message.contains("ready: delivery conditions pass"));

        check.after.push(deliver::AfterConditionCheck {
            address: "@planner".to_owned(),
            met: false,
            met_at: None,
            agent_present: false,
            status: None,
        });
        assert!(!check.passes());
        assert_eq!(
            render_verdict(&check.verdict(), "@claude", None, Timestamp::now()),
            "waiting on @planner to finish (@planner not running)"
        );

        check.schedule.ready = true;
        check.after.clear();
        check.agent.present = false;
        check.gate.open = false;
        assert_eq!(
            delivery_action_hint(&check.verdict(), &message_id, "@claude"),
            Some("force now: rimz message steer msg_0000000000000001".to_owned())
        );
        check.expiry = deliver::ExpiryCheck {
            expired: true,
            expires_at: Some(Timestamp::UNIX_EPOCH),
        };
        check.ask.waiting = true;
        check.agent.present = false;
        assert!(
            steer_failure(&check, "@claude", &message_id)
                .starts_with("expired: automatic command valid for 10m")
        );
        assert_eq!(
            delivery_action_hint(&check.verdict(), &message_id, "@claude"),
            None
        );
    }

    #[test]
    fn receiver_gone_names_a_surviving_audit_record() {
        let now = Timestamp::from_second(120).expect("fixed now");
        let receiver = rimz::testkit::agent_state(
            "codex",
            "sess-resumed",
            Timestamp::from_second(60).expect("fixed last seen"),
        );

        assert_eq!(
            render_verdict(
                &deliver::DeliveryVerdict::ReceiverGone,
                "@coder#lane",
                Some(&receiver),
                now,
            ),
            "stuck: receiver @coder#lane is gone; its session record survives (last seen 1m ago (1970-01-01T00:01:00Z)), but no live process claims it; try `rimz agents resume`"
        );
        assert_eq!(
            render_verdict(
                &deliver::DeliveryVerdict::ReceiverGone,
                "@coder#lane",
                None,
                now,
            ),
            "stuck: receiver @coder#lane is gone"
        );
    }
}
