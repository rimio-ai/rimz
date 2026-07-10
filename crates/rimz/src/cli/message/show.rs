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
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let store = open_store(&workspace)?;
    let cached_snapshot = store.snapshot_cached().context("reading agent snapshot")?;
    let Some(message) = projected_messages(&store)?
        .into_iter()
        .find(|message| message.message_id == message_id)
    else {
        bail!("message {message_id} not found");
    };
    let timeline = message_timeline(&store, &message_id)?;
    let live_messages = store.list_messages()?;
    let now = Timestamp::now();
    let delivery = if message.status.is_open() {
        match live_messages
            .iter()
            .find(|record| record.message_id == message.message_id)
        {
            Some(record) => {
                let mut snapshot = crate::cli::resolution_snapshot(&workspace, &store, globals)?;
                if let Ok(runtime) = rimz::RuntimePaths::for_workspace(record.workspace_id.clone())
                {
                    snapshot =
                        snapshot.with_agent_context(rimz::store::agent_context::read_all(&runtime));
                }
                let check = deliver::explain(record, &live_messages, &snapshot, now);
                let agents: Vec<&AgentState> = snapshot.root_agents().collect();
                let target = message_target(&message, &agents);
                Some(MessageDeliveryJson {
                    verdict: delivery_verdict(&check, &target, now),
                    check,
                })
            }
            None => None,
        }
    } else {
        None
    };
    if json {
        let rendered = serde_json::to_string_pretty(&MessageShowJson {
            message,
            timeline,
            delivery,
        })?;
        #[expect(clippy::print_stdout, reason = "json emitter")]
        {
            println!("{rendered}");
        }
        return Ok(());
    }

    let agents: Vec<&AgentState> = cached_snapshot.root_agents().collect();
    let raw_target = message_target(&message, &agents);
    let target = scoped_handle(raw_target.clone(), message.channel.as_deref());
    let sender = scoped_handle(message.sender.render(), message.channel.as_deref());
    let mut out = render::out();
    writeln!(
        out,
        "{} — {}",
        render::paint(render::palette::ACCENT.bold(), message.message_id.as_str()),
        render::paint(
            render::status::message(message.status),
            message.status.as_str()
        )
    )?;
    let mut kv = render::KeyVals::new().indent(2);
    kv.push("from", render::cell(sender).fg(render::palette::META));
    kv.push("to", render::cell(target.clone()).fg(render::palette::META));
    kv.push(
        "channel",
        render::cell(message.channel.clone().unwrap_or_else(|| "-".to_owned())).dash(),
    );
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
                    .map(|condition| {
                        if condition.met_at.is_some() {
                            format!("{} ✓", condition.address)
                        } else {
                            format!("{} waiting", condition.address)
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
    kv.render(&mut out)?;
    writeln!(out)?;
    writeln!(out, "{}", render::paint(render::palette::HEADER, "TEXT"))?;
    if let Some(text) = message.text.as_deref() {
        write_indented_block(&mut out, text)?;
    } else {
        writeln!(out, "  ({})", textless_location(&message, &raw_target))?;
    }
    writeln!(out)?;
    writeln!(
        out,
        "{}",
        render::paint(render::palette::HEADER, "TIMELINE")
    )?;
    if timeline.is_empty() {
        writeln!(out, "  -")?;
    } else {
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
        for event in &timeline {
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
        table.render(&mut out)?;
    }
    if let Some(delivery) = delivery {
        render_delivery_check(
            &mut out,
            &message.message_id,
            &delivery.check,
            &delivery.verdict,
            now,
        )?;
    }
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
    if delivery_conditions_pass(check) {
        return format!(
            "{message_id} has a recent delivery attempt in progress; retry in a few seconds"
        );
    }
    delivery_verdict(check, target, Timestamp::now())
}

pub(super) fn delivery_conditions_pass(check: &deliver::DeliveryCheck) -> bool {
    check.schedule.ready
        && check.after.iter().all(|condition| condition.met)
        && check.fifo.head
        && check.agent.present
        && gate_ready(check)
        && !check.ask.waiting
        && check.pane.present
}

pub(super) fn time_with_absolute(ts: Timestamp, now: Timestamp) -> String {
    let absolute = ts.strftime("%Y-%m-%dT%H:%M:%SZ");
    format!("{} ({absolute})", render::rel_age(ts, now))
}

pub(super) fn time_until_with_absolute(ts: Timestamp, now: Timestamp) -> String {
    let absolute = ts.strftime("%Y-%m-%dT%H:%M:%SZ");
    format!("{} ({absolute})", render::rel_until(ts, now))
}

pub(super) fn write_indented_block(out: &mut impl Write, text: &str) -> Result<()> {
    if text.is_empty() {
        writeln!(out, "  ")?;
        return Ok(());
    }
    for line in text.split('\n') {
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
    now: Timestamp,
) -> Result<()> {
    writeln!(out)?;
    writeln!(
        out,
        "{}",
        render::paint(render::palette::HEADER, "DELIVERY CHECK")
    )?;
    let mut kv = render::KeyVals::new().indent(2);
    let schedule = if check.schedule.ready {
        match check.schedule.retry_after {
            Some(retry_after) if retry_after > now => {
                format!(
                    "ok; retry wake {}",
                    time_until_with_absolute(retry_after, now)
                )
            }
            Some(retry_after) => format!("ok; retry wake {}", time_with_absolute(retry_after, now)),
            None => "ok".to_owned(),
        }
    } else {
        check
            .schedule
            .not_before
            .map(|not_before| format!("opens {}", time_until_with_absolute(not_before, now)))
            .unwrap_or_else(|| "not ready".to_owned())
    };
    kv.push("schedule", condition_cell(check.schedule.ready, schedule));
    if !check.after.is_empty() {
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
        kv.push("after", condition_cell(ready, detail));
    }
    let fifo = if check.fifo.head {
        "ok".to_owned()
    } else {
        check
            .fifo
            .blocker
            .as_ref()
            .map(|blocker| format!("behind {blocker}"))
            .unwrap_or_else(|| "head unavailable".to_owned())
    };
    kv.push("fifo", condition_cell(check.fifo.head, fifo));
    kv.push(
        "agent",
        condition_cell(
            check.agent.present,
            if check.agent.present {
                "ok".to_owned()
            } else {
                "receiver gone".to_owned()
            },
        ),
    );
    let gate_ready = gate_ready(check);
    let gate = if check.gate.resume_recovered == Some(false) {
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
    kv.push("gate", condition_cell(gate_ready, gate));
    let ask = if !check.ask.waiting {
        if check.ask.force {
            "ok (--force)".to_owned()
        } else {
            "ok".to_owned()
        }
    } else {
        "waiting in pane".to_owned()
    };
    kv.push("ask", condition_cell(!check.ask.waiting, ask));
    let pane = if check.pane.present {
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
    kv.push("pane", condition_cell(check.pane.present, pane));
    kv.render(out)?;
    writeln!(out, "  {verdict}")?;
    if let Some(hint) = delivery_action_hint(check, message_id) {
        writeln!(out, "  {}", render::paint(render::palette::FAINT, &hint))?;
    }
    Ok(())
}

pub(super) fn condition_cell(ok: bool, text: String) -> render::Cell {
    let style = if ok {
        render::palette::GOOD
    } else {
        render::palette::WARN
    };
    render::cell(text).fg(style)
}

pub(super) fn delivery_verdict(
    check: &deliver::DeliveryCheck,
    target: &str,
    now: Timestamp,
) -> String {
    if !check.schedule.ready {
        return check
            .schedule
            .not_before
            .map(|not_before| {
                format!(
                    "scheduled: opens {}",
                    time_until_with_absolute(not_before, now)
                )
            })
            .unwrap_or_else(|| "scheduled: waiting for readiness floor".to_owned());
    }
    if let Some(condition) = check.after.iter().find(|condition| !condition.met) {
        return if condition.agent_present {
            format!("waiting on {} to finish", condition.address)
        } else {
            format!(
                "waiting on {} to finish ({} not running)",
                condition.address, condition.address
            )
        };
    }
    if !check.fifo.head {
        return check
            .fifo
            .blocker
            .as_ref()
            .map(|blocker| format!("blocked: behind {blocker}"))
            .unwrap_or_else(|| "blocked: FIFO head unavailable".to_owned());
    }
    if !check.agent.present {
        return format!("stuck: receiver {target} is gone");
    }
    if !check.gate.open {
        let status = check
            .gate
            .status
            .map(|status| status.as_str())
            .unwrap_or("unknown");
        if check.gate.gate == DeliveryGate::Resume {
            return format!(
                "waiting: {target} is {status}; resume gate opens when the agent is paused and provider recovery passes"
            );
        }
        return format!(
            "waiting: {target} is {status}; gate '{}' opens at next turn end",
            check.gate.gate
        );
    }
    if check.gate.resume_recovered == Some(false) {
        return format!("waiting: {target} is paused; resume gate opens after provider recovery");
    }
    if check.ask.waiting {
        return format!("waiting: {target} is waiting on input in its pane");
    }
    if !check.pane.present {
        return match &check.pane.pinned_pane_id {
            Some(pane_id) => format!("stuck: pinned pane {pane_id} is not live for {target}"),
            None => format!("stuck: no live pane for {target}"),
        };
    }
    "ready: delivery conditions pass".to_owned()
}

pub(super) fn delivery_action_hint(
    check: &deliver::DeliveryCheck,
    message_id: &MessageId,
) -> Option<String> {
    if !check.schedule.ready {
        return Some(format!(
            "force now: rimz message steer {message_id}  ·  or: rimz message edit {message_id} --no-schedule"
        ));
    }
    if check.after.iter().any(|condition| !condition.met) {
        return Some(format!("force now: rimz message steer {message_id}"));
    }
    if !check.fifo.head || !gate_ready(check) {
        return Some(format!("force now: rimz message steer {message_id}"));
    }
    if check.ask.waiting {
        return Some(format!(
            "force now: rimz message steer {message_id} --force"
        ));
    }
    None
}

pub(super) fn gate_ready(check: &deliver::DeliveryCheck) -> bool {
    check.gate.open && check.gate.resume_recovered != Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    use rimz::agents::AgentStatus;
    use rimz::ids::{MessageId, MuxName, PaneId};

    #[test]
    fn delivery_checks_report_recent_attempt_and_after_blocker() {
        let message_id = MessageId::parse("msg_0000000000000001").unwrap();
        let mut check = deliver::DeliveryCheck {
            schedule: deliver::ScheduleCheck {
                ready: true,
                not_before: None,
                retry_after: None,
            },
            after: Vec::new(),
            fifo: deliver::FifoCheck {
                head: true,
                blocker: None,
            },
            agent: deliver::AgentCheck { present: true },
            gate: deliver::GateCheck {
                gate: DeliveryGate::Done,
                status: Some(AgentStatus::Idle),
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
        assert!(!delivery_conditions_pass(&check));
        assert_eq!(
            delivery_verdict(&check, "@claude", Timestamp::now()),
            "waiting on @planner to finish (@planner not running)"
        );
    }
}
