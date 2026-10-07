use super::*;

pub(super) enum LaneScope {
    All,
    Root,
    Named(String),
}

impl LaneScope {
    fn contains_channel(&self, channel: Option<&str>, root_channel: Option<&str>) -> bool {
        match self {
            Self::All => true,
            Self::Root => rimz::address::record_in_root_lane(channel, root_channel),
            Self::Named(name) => channel == Some(name.as_str()),
        }
    }

    fn named(&self) -> Option<&str> {
        match self {
            Self::Named(channel) => Some(channel),
            Self::All | Self::Root => None,
        }
    }

    fn includes_archived(&self) -> bool {
        matches!(self, Self::All)
    }
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct MessageListRow {
    pub(super) message_id: MessageId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) address: Option<String>,
    pub(super) kind: AgentKind,
    pub(super) agent_id: AgentSessionId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) agent_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) channel: Option<String>,
    pub(super) sender: MessageSender,
    pub(super) body: MessageBody,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) text: Option<String>,
    pub(super) enter: bool,
    pub(super) gate: DeliveryGate,
    pub(super) force: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) pane_id: Option<PaneId>,
    pub(super) status: MessageStatus,
    pub(super) enqueued_at: Timestamp,
    pub(super) updated_at: Timestamp,
    pub(super) attempts: u32,
    pub(super) unconfirmed_sends: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) last_attempt_at: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) delivered_at: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) not_before: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) after: Vec<AfterCondition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) when: Vec<WhenCondition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) retry_after: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) auto_compact: Option<AutoCompact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) compacted_context_tokens: Option<u64>,
}

impl MessageListRow {
    pub(super) fn from_record(message: MessageRecord) -> Self {
        Self {
            message_id: message.message_id,
            address: message.address,
            kind: message.kind,
            agent_id: message.agent_id,
            agent_name: message.agent_name,
            channel: message.channel,
            sender: message.sender,
            body: message.body,
            text: Some(message.text),
            enter: message.enter,
            gate: message.gate,
            force: message.force,
            pane_id: message.pane_id,
            status: message.status,
            enqueued_at: message.enqueued_at,
            updated_at: message.updated_at,
            attempts: message.attempts,
            unconfirmed_sends: message.unconfirmed_sends,
            last_attempt_at: message.last_attempt_at,
            last_error: message.last_error,
            delivered_at: message.delivered_at,
            not_before: message.not_before,
            after: message.after,
            when: message.when,
            retry_after: message.retry_after,
            auto_compact: message.auto_compact,
            compacted_context_tokens: message.compacted_context_tokens,
        }
    }

    pub(super) fn from_terminal_event(
        event: &EventEnvelope,
        payload: MessageEventPayload,
    ) -> Option<Self> {
        if !payload.status.is_terminal() {
            return None;
        }
        let delivered_at = payload
            .delivered_at
            .or_else(|| (payload.status == MessageStatus::Delivered).then_some(event.timestamp));
        Some(Self {
            message_id: payload.message_id,
            address: payload.address,
            kind: payload.kind,
            agent_id: payload.agent_id,
            agent_name: payload.agent_name,
            channel: payload.channel,
            sender: payload.sender.unwrap_or_default(),
            body: payload.body,
            text: None,
            enter: payload.enter,
            gate: payload.gate,
            force: payload.forced,
            pane_id: payload.pane_id,
            status: payload.status,
            enqueued_at: payload.enqueued_at.unwrap_or(event.timestamp),
            updated_at: event.timestamp,
            attempts: payload.attempts,
            unconfirmed_sends: payload.unconfirmed_sends,
            last_attempt_at: None,
            last_error: payload.reason,
            delivered_at,
            not_before: None,
            after: Vec::new(),
            when: Vec::new(),
            retry_after: None,
            auto_compact: None,
            compacted_context_tokens: payload.compacted_context_tokens,
        })
    }
}

pub(super) fn list_messages(args: ListArgs, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let store = &ctx.store;
    let snapshot = ctx.cached_snapshot()?;
    let mut messages = projected_messages(store)?;
    let inline_channel = args
        .target
        .as_deref()
        .map(|raw| {
            rimz::address::require_mention(raw)?;
            rimz::address::parse_selector(raw).map(|(_, channel)| channel)
        })
        .transpose()?
        .flatten();
    let channel = rimz::address::reconcile_channel(
        args.target.as_deref().unwrap_or_default(),
        inline_channel.as_deref(),
        args.channel.as_deref(),
        if args.all { None } else { ctx.channel() },
    )?;
    let explicit = inline_channel.as_deref().or(args.channel.as_deref());
    if let Some(explicit) = explicit {
        require_known_channel(explicit, &messages, &snapshot, &ctx)?;
    }
    let mut lane_scope = if args.all {
        LaneScope::All
    } else if explicit.is_some_and(|channel| {
        rimz::address::is_root_lane_filter(channel, &ctx.workspace.project_root)
    }) || (explicit.is_none()
        && ctx.address_context().origin == rimz::address::ChannelOrigin::Root)
    {
        LaneScope::Root
    } else {
        channel.map_or(LaneScope::Root, LaneScope::Named)
    };
    let root_channel = rimz::address::root_lane_channel(&ctx.workspace.project_root);
    let target = if let Some(raw) = args.target.as_deref() {
        let mut context = ctx.address_context();
        if args.all {
            context.channel = None;
        }
        let agent = crate::cli::resolve_agent_one(
            store,
            &snapshot,
            raw,
            args.channel.as_deref(),
            &context,
        )?;
        if !args.all {
            let channel = agent.channel();
            if agent.root_lane {
                lane_scope = LaneScope::Root;
            } else {
                lane_scope = channel.map_or(LaneScope::Root, LaneScope::Named);
            }
        }
        Some(agent)
    } else {
        None
    };
    let unfiltered_lane = !args.all && args.status.is_none() && args.target.is_none();
    let empty = unfiltered_lane
        .then(|| other_lanes_line(&messages, &lane_scope, root_channel.as_deref(), args.system))
        .flatten()
        .unwrap_or_else(|| empty_message_digest(&lane_scope, args.status));
    if let Some(agent) = target {
        messages.retain(|message| {
            rimz::agents::AgentCardRef::new(
                &message.kind,
                &message.agent_id,
                message.agent_name.as_deref(),
            )
            .matches(agent.card_ref())
        });
    }
    messages.retain(|message| {
        lane_scope.contains_channel(message.channel.as_deref(), root_channel.as_deref())
    });
    if let Some(status) = args.status {
        messages.retain(|message| message.status == status);
    } else if !lane_scope.includes_archived() {
        messages.retain(|message| message.status != MessageStatus::Archived);
    }
    let system_hidden = if args.system {
        0
    } else {
        retain_conversation(&mut messages)
    };
    messages.sort_by(|a, b| {
        b.enqueued_at
            .cmp(&a.enqueued_at)
            .then_with(|| b.message_id.as_str().cmp(a.message_id.as_str()))
    });
    let limit = args.limit.unwrap_or(DEFAULT_MESSAGE_LIST_LIMIT);
    let hidden = if limit == 0 {
        0
    } else {
        messages.len().saturating_sub(limit)
    };
    if limit != 0 {
        messages.truncate(limit);
    }
    if args.json {
        render::json_pretty(&messages)?;
    } else {
        let agents = rimz::address::addressable_agents(&snapshot);
        let mut out = render::out();
        render_message_digest(
            &mut out,
            messages,
            &agents,
            &lane_scope,
            root_channel.as_deref(),
            hidden,
            system_hidden,
            &empty,
        )?;
    }
    Ok(())
}

fn require_known_channel(
    explicit: &str,
    messages: &[MessageListRow],
    snapshot: &SidebarSnapshot,
    ctx: &Ctx,
) -> Result<()> {
    if rimz::address::is_root_lane_filter(explicit, &ctx.workspace.project_root) {
        return Ok(());
    }
    let root_channel = rimz::address::root_lane_channel(&ctx.workspace.project_root);
    let mut known = messages
        .iter()
        .filter_map(|message| {
            rimz::address::record_lane_label(message.channel.as_deref(), root_channel.as_deref())
                .map(str::to_owned)
        })
        .collect::<std::collections::BTreeSet<_>>();
    known.extend(
        snapshot
            .agents
            .iter()
            .filter(|agent| agent.ended_at.is_none())
            .filter_map(|agent| agent.lane_label()),
    );
    known.extend(
        rimz::worktree::discover_owned(&ctx.workspace.project_root)
            .unwrap_or_default()
            .into_iter()
            .map(|worktree| worktree.marker.name),
    );
    if known.contains(explicit) {
        return Ok(());
    }
    bail!(
        "no channel #{explicit}\nknown channels: {}",
        known_channels_line(&known)
    );
}

fn known_channels_line(known: &std::collections::BTreeSet<String>) -> String {
    const SHOWN: usize = 20;
    if known.is_empty() {
        return "(none)".to_owned();
    }
    let mut line = known
        .iter()
        .take(SHOWN)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    if known.len() > SHOWN {
        line.push_str(&format!(", …and {} more", known.len() - SHOWN));
    }
    line
}

/// The empty-lane line that counts what other lanes hold, or `None` when they hold nothing.
fn other_lanes_line(
    messages: &[MessageListRow],
    lane_scope: &LaneScope,
    root_channel: Option<&str>,
    system: bool,
) -> Option<String> {
    let others = messages
        .iter()
        .filter(|message| {
            !lane_scope.contains_channel(message.channel.as_deref(), root_channel)
                && message.status != MessageStatus::Archived
                && (system || message.sender.is_conversation())
        })
        .collect::<Vec<_>>();
    if others.is_empty() {
        return None;
    }
    let lanes = others
        .iter()
        .map(|message| rimz::address::record_lane_label(message.channel.as_deref(), root_channel))
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let undelivered = others
        .iter()
        .filter(|message| message.status == MessageStatus::Queued)
        .count();
    let lane = lane_scope
        .named()
        .map_or_else(|| "#main".to_owned(), |channel| format!("#{channel}"));
    let hint = if undelivered > 0 {
        format!(", {undelivered} not yet delivered — rimz message list --all --status queued")
    } else {
        " — rimz message list --all".to_owned()
    };
    Some(format!(
        "no messages in {lane}. {} in {lanes} other lane{}{hint}",
        others.len(),
        if lanes == 1 { "" } else { "s" }
    ))
}

fn retain_conversation(messages: &mut Vec<MessageListRow>) -> usize {
    let total = messages.len();
    messages.retain(|message| message.sender.is_conversation());
    total - messages.len()
}

pub(super) fn projected_messages(store: &rimz::Store) -> Result<Vec<MessageListRow>> {
    let mut rows = std::collections::BTreeMap::new();
    for event in store.read_events()? {
        let EventKind::Message { payload, .. } = event.kind() else {
            continue;
        };
        let Some(row) = MessageListRow::from_terminal_event(&event, payload) else {
            continue;
        };
        rows.insert(row.message_id.to_string(), row);
    }
    for message in store.list_message_history()? {
        let row = MessageListRow::from_record(message);
        rows.insert(row.message_id.to_string(), row);
    }
    for message in store.list_messages()? {
        let row = MessageListRow::from_record(message);
        rows.insert(row.message_id.to_string(), row);
    }
    Ok(rows.into_values().collect())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_message_digest(
    out: &mut impl Write,
    messages: Vec<MessageListRow>,
    agents: &[&AgentState],
    lane_scope: &LaneScope,
    root_channel: Option<&str>,
    hidden: usize,
    system_hidden: usize,
    empty: &str,
) -> Result<()> {
    let now = Timestamp::now();
    if messages.is_empty() {
        writeln!(out, "{}", render::paint(render::palette::faint(), empty))?;
    } else if matches!(lane_scope, LaneScope::All) {
        for (index, (channel, rows)) in message_digest_groups(messages, root_channel)
            .into_iter()
            .enumerate()
        {
            if index > 0 {
                writeln!(out)?;
            }
            writeln!(
                out,
                "{}",
                render::paint(render::palette::header(), &lane_header(channel.as_deref()))
            )?;
            render_message_rows(out, rows, agents, root_channel, now, 2, 4)?;
        }
    } else {
        render_message_rows(out, messages, agents, root_channel, now, 0, 2)?;
    }
    if hidden > 0 {
        writeln!(
            out,
            "... {hidden} older messages hidden (--limit 0 for all)"
        )?;
    }
    if system_hidden > 0 {
        writeln!(
            out,
            "... {system_hidden} system messages hidden (--system shows them)"
        )?;
    }
    Ok(())
}

pub(super) fn render_message_rows(
    out: &mut impl Write,
    messages: Vec<MessageListRow>,
    agents: &[&AgentState],
    root_channel: Option<&str>,
    now: Timestamp,
    row_indent: usize,
    snippet_indent: usize,
) -> Result<()> {
    let row_pad = " ".repeat(row_indent);
    let snippet_pad = " ".repeat(snippet_indent);
    let snippet_width = render::terminal_columns(120).saturating_sub(snippet_indent);
    for message in messages {
        let lane = rimz::address::record_lane_label(message.channel.as_deref(), root_channel);
        let target = scoped_handle(message_target(&message, agents, root_channel), lane);
        let sender = scoped_handle(message_sender(&message.sender, agents, root_channel), lane);
        writeln!(
            out,
            "{row_pad}{}{}{}  {}  {}  {}",
            rendered_sender(&message.sender, &sender),
            render::paint(render::palette::faint(), " → "),
            render::paint(render::palette::meta().bold(), &target),
            render::paint(
                render::status::message(message.status),
                message.status.as_str()
            ),
            render::rel_age(message.enqueued_at, now),
            render::paint(render::palette::faint(), message.message_id.as_str())
        )?;
        writeln!(
            out,
            "{snippet_pad}{}",
            message_snippet(&message, snippet_width)
        )?;
    }
    Ok(())
}

pub(super) fn empty_message_digest(
    lane_scope: &LaneScope,
    status: Option<MessageStatus>,
) -> String {
    let qualifier = status.map(MessageStatus::as_str).unwrap_or_default();
    let kind = if qualifier.is_empty() {
        "messages".to_owned()
    } else {
        format!("{qualifier} messages")
    };
    let mut line = match lane_scope {
        LaneScope::All => format!("no {kind}"),
        LaneScope::Root => format!("no {kind} in #main"),
        LaneScope::Named(channel) => format!("no {kind} in {}", lane_header(Some(channel))),
    };
    if !matches!(lane_scope, LaneScope::All) {
        line.push_str(" — rimz message list --all shows every channel");
    }
    line
}

pub(super) fn message_digest_groups(
    messages: Vec<MessageListRow>,
    root_channel: Option<&str>,
) -> Vec<(Option<String>, Vec<MessageListRow>)> {
    let mut groups: Vec<(Option<String>, Vec<MessageListRow>)> = Vec::new();
    for message in messages {
        let lane = rimz::address::record_lane_label(message.channel.as_deref(), root_channel)
            .map(str::to_owned);
        if let Some((_, rows)) = groups.iter_mut().find(|(channel, _)| channel == &lane) {
            rows.push(message);
        } else {
            groups.push((lane, vec![message]));
        }
    }
    groups
}

pub(super) fn rendered_sender(sender: &MessageSender, rendered: &str) -> String {
    match sender {
        MessageSender::Human => render::paint(render::palette::cool(), rendered),
        MessageSender::Agent { .. }
        | MessageSender::Harness { .. }
        | MessageSender::Subagent { .. }
        | MessageSender::System => render::paint(render::palette::meta().bold(), rendered),
    }
}

pub(super) fn lane_header(channel: Option<&str>) -> String {
    channel
        .filter(|channel| !channel.is_empty())
        .map(|channel| format!("#{channel}"))
        .unwrap_or_else(|| "#main".to_owned())
}

pub(super) fn message_target(
    message: &MessageListRow,
    agents: &[&AgentState],
    root_channel: Option<&str>,
) -> String {
    let channel = rimz::address::record_lane_label(message.channel.as_deref(), root_channel);
    address::message_target(
        message.address.as_deref(),
        &message.kind,
        &message.agent_id,
        message.agent_name.as_deref(),
        channel,
        agents,
    )
}

pub(super) fn message_sender(
    sender: &MessageSender,
    agents: &[&AgentState],
    root_channel: Option<&str>,
) -> String {
    let mut sender = sender.clone();
    if let MessageSender::Agent { channel, .. } = &mut sender {
        *channel =
            rimz::address::record_lane_label(channel.as_deref(), root_channel).map(str::to_owned);
    }
    rimz::address::agent_sender_handle(&sender, agents, None).unwrap_or_else(|| sender.render())
}

pub(super) fn scoped_handle(rendered: String, filter_channel: Option<&str>) -> String {
    let Some(filter) = filter_channel else {
        return rendered;
    };
    let Some((base, channel)) = rendered.rsplit_once('#') else {
        return rendered;
    };
    if channel == filter {
        base.to_owned()
    } else {
        rendered
    }
}

pub(super) fn message_snippet(message: &MessageListRow, width: usize) -> String {
    let after = message
        .after
        .iter()
        .filter(|condition| condition.met_at.is_none())
        .map(|condition| condition.address.as_str())
        .collect::<Vec<_>>();
    let marker = (!after.is_empty()).then(|| format!("after {}", after.join(", ")));
    let when = message
        .when
        .iter()
        .filter(|condition| condition.met_at.is_none())
        .map(|condition| {
            format!(
                "when {} {} {}",
                condition.address,
                condition.status.as_str(),
                rimz::store::message::format_dwell(condition.dwell_secs)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let marker = match (marker, when.is_empty()) {
        (Some(after), false) => Some(format!("{after} · {when}")),
        (marker, true) => marker,
        (None, false) => Some(when),
    };
    if let Some(text) = message.text.as_deref() {
        let text = collapse_home_in_snippet(text);
        return preview(
            &marker.map_or(text.clone(), |marker| format!("{text} · {marker}")),
            width,
        );
    }
    if let Some(reason) = message
        .last_error
        .as_deref()
        .filter(|reason| !reason.is_empty())
    {
        let reason = collapse_home_in_snippet(reason);
        let detail = marker.map_or(reason.clone(), |marker| format!("{reason} · {marker}"));
        return render::paint(render::palette::faint(), &preview(&detail, width));
    }
    render::paint(
        render::palette::faint(),
        &marker.unwrap_or_else(|| "(text no longer kept)".to_owned()),
    )
}

pub(super) fn collapse_home_in_snippet(text: &str) -> String {
    let home = std::env::var("HOME").ok();
    collapse_home_in_snippet_to(home.as_deref(), text)
}

pub(super) fn collapse_home_in_snippet_to(home: Option<&str>, text: &str) -> String {
    let Some(home) = home
        .map(|home| home.trim_end_matches('/'))
        .filter(|home| !home.is_empty() && *home != "/")
    else {
        return text.to_owned();
    };
    let mut collapsed = String::new();
    let mut rest = text;
    let mut changed = false;
    while let Some(index) = rest.find(home) {
        let (before, matched) = rest.split_at(index);
        let after = &matched[home.len()..];
        if home_match_boundary(before.chars().next_back(), after.chars().next()) {
            collapsed.push_str(before);
            collapsed.push('~');
            rest = after;
            changed = true;
        } else {
            let (head, tail) = matched.split_at(matched.chars().next().unwrap().len_utf8());
            collapsed.push_str(before);
            collapsed.push_str(head);
            rest = tail;
        }
    }
    if !changed {
        return text.to_owned();
    }
    collapsed.push_str(rest);
    collapsed
}

pub(super) fn home_match_boundary(previous: Option<char>, next: Option<char>) -> bool {
    home_start_boundary(previous) && home_end_boundary(next)
}

pub(super) fn home_start_boundary(ch: Option<char>) -> bool {
    ch.is_none_or(|ch| {
        ch.is_whitespace() || matches!(ch, '"' | '\'' | '`' | '(' | '[' | '{' | '<' | '=' | ':')
    })
}

pub(super) fn home_end_boundary(ch: Option<char>) -> bool {
    ch.is_none_or(|ch| {
        ch.is_whitespace()
            || matches!(
                ch,
                '/' | '"' | '\'' | '`' | ')' | ']' | '}' | '>' | ',' | ';' | ':'
            )
    })
}

pub(super) fn preview(text: &str, width: usize) -> String {
    let preview = text.replace(['\r', '\n', '\t'], " ");
    if preview.width() <= width {
        return preview;
    }
    if width == 0 {
        return String::new();
    }
    if width <= 3 {
        return ".".repeat(width);
    }
    let mut shortened = String::new();
    let mut used = 0;
    for ch in preview.chars() {
        let char_width = ch.width().unwrap_or(0);
        if used + char_width > width - 3 {
            break;
        }
        shortened.push(ch);
        used += char_width;
    }
    shortened.push_str("...");
    shortened
}

#[cfg(test)]
mod tests {
    use super::*;

    use rimz::agents::AgentStatus;
    use rimz::ids::{AgentKind, MuxName, PaneId, WorkspaceId};
    use rimz::pane::PaneRef;

    #[test]
    fn message_snippet_names_text_not_kept() {
        let mut row = MessageListRow::from_record(MessageRecord::new(
            workspace_id(),
            &agent("sess-coder", AgentStatus::Idle),
            "text".to_owned(),
            DeliveryGate::Done,
        ));
        row.text = None;
        assert!(message_snippet(&row, 120).contains("(text no longer kept)"));
    }

    #[test]
    fn message_target_keeps_single_sigil() {
        let mut coder = agent("sess-coder", AgentStatus::Idle);
        coder.role = Some("coder".to_owned());
        let snapshot = SidebarSnapshot::build_with_agents(workspace_id(), vec![coder], now());
        let message = MessageRecord::new(
            workspace_id(),
            &snapshot.agents[0],
            "work".to_owned(),
            DeliveryGate::Done,
        );
        let message = MessageListRow::from_record(message);
        let agents = rimz::address::addressable_agents(&snapshot);
        assert_eq!(message_target(&message, &agents, None), "@coder#project");
    }

    #[test]
    fn message_snippet_marks_only_unmet_after_conditions() {
        let upstream = agent("sess-planner", AgentStatus::Running);
        let condition = AfterCondition {
            kind: upstream.kind.clone(),
            agent_id: upstream.agent_id.clone(),
            agent_name: upstream.name.clone(),
            address: "@planner".to_owned(),
            met_at: None,
        };
        let receiver = agent("sess-coder", AgentStatus::Running);
        let waiting = MessageListRow::from_record(
            MessageRecord::new(
                workspace_id(),
                &receiver,
                "read plan".to_owned(),
                DeliveryGate::Done,
            )
            .with_after(vec![condition.clone()]),
        );
        let met = MessageListRow::from_record(
            MessageRecord::new(
                workspace_id(),
                &receiver,
                "read plan".to_owned(),
                DeliveryGate::Done,
            )
            .with_after(vec![AfterCondition {
                met_at: Some(now()),
                ..condition
            }]),
        );

        assert_eq!(message_snippet(&waiting, 120), "read plan · after @planner");
        assert_eq!(message_snippet(&met, 120), "read plan");
    }

    #[test]
    fn message_target_uses_stored_address_before_fallbacks() {
        let message = MessageRecord::new(
            workspace_id(),
            &agent("sess-coder", AgentStatus::Idle),
            "work".to_owned(),
            DeliveryGate::Done,
        )
        .with_channel(Some("project".to_owned()))
        .with_address(Some("@saved#project".to_owned()));
        let message = MessageListRow::from_record(message);

        assert_eq!(message_target(&message, &[], None), "@saved#project");
    }

    #[test]
    fn message_target_falls_back_to_agent_name_and_channel_when_agent_is_gone() {
        let message = MessageRecord::new(
            workspace_id(),
            &agent("sess-coder", AgentStatus::Idle),
            "work".to_owned(),
            DeliveryGate::Done,
        )
        .with_channel(Some("project".to_owned()));
        let message = MessageListRow::from_record(message);

        assert_eq!(
            message_target(&message, &[], None),
            "@sess-coder-name#project"
        );
    }

    #[test]
    fn message_target_falls_back_to_kind_id_for_nameless_records() {
        let mut receiver = agent("sess-coder", AgentStatus::Idle);
        receiver.name = None;
        let message = MessageRecord::new(
            workspace_id(),
            &receiver,
            "work".to_owned(),
            DeliveryGate::Done,
        );
        let message = MessageListRow::from_record(message);

        assert_eq!(message_target(&message, &[], None), "claude:sess-coder");
    }

    #[test]
    fn scoped_handle_drops_matching_lane_suffix() {
        assert_eq!(
            scoped_handle("@coder#project".to_owned(), Some("project")),
            "@coder"
        );
        // Lane membership is exact: a team lane keeps its suffix under the
        // directory filter.
        assert_eq!(
            scoped_handle("@coder#project/forge".to_owned(), Some("project")),
            "@coder#project/forge"
        );
        assert_eq!(
            scoped_handle("@coder#ops".to_owned(), Some("project")),
            "@coder#ops"
        );
        assert_eq!(scoped_handle("you".to_owned(), Some("project")), "you");
    }

    #[test]
    fn preview_respects_width_and_flattens_control_whitespace() {
        assert_eq!(preview("a\nb\tc", 10), "a b c");
        assert_eq!(preview("abcdef", 4), "a...");
        assert_eq!(preview("abcdef", 3), "...");
    }

    #[test]
    fn message_digest_groups_all_lanes_once_by_latest_activity() {
        let output = render_digest(
            vec![
                message_row("sess-docs-new", Some("docs"), "new docs"),
                message_row("sess-ops", Some("ops"), "ops"),
                message_row("sess-docs-old", Some("docs"), "old docs"),
            ],
            LaneScope::All,
            None,
        );

        assert_eq!(output.matches("#docs").count(), 1);
        assert_eq!(output.matches("#ops").count(), 1);
        assert!(output.find("#docs").unwrap() < output.find("new docs").unwrap());
        assert!(output.find("new docs").unwrap() < output.find("old docs").unwrap());
        assert!(output.find("old docs").unwrap() < output.find("#ops").unwrap());

        let lines: Vec<&str> = output.lines().collect();
        let snippet = lines
            .iter()
            .position(|line| line.contains("new docs"))
            .unwrap();
        assert!(lines[snippet - 1].starts_with("  "));
        assert!(lines[snippet].starts_with("    "));
    }

    #[test]
    fn root_scope_includes_legacy_and_basename_records() {
        assert!(LaneScope::Root.contains_channel(None, Some("project")));
        assert!(LaneScope::Root.contains_channel(Some("project"), Some("project")));
        assert!(!LaneScope::Root.contains_channel(Some("docs"), Some("project")));
    }

    #[test]
    fn all_digest_groups_root_records_under_main_once() {
        let output = render_digest(
            vec![
                message_row("root", Some("project"), "root text"),
                message_row("legacy", None, "legacy text"),
            ],
            LaneScope::All,
            None,
        );
        assert_eq!(output.matches("#main").count(), 1, "{output}");
        assert!(!output.contains("#project"), "{output}");
    }

    #[test]
    fn message_digest_scopes_handles_by_row_lane() {
        let output = render_digest(
            vec![
                message_row_with_sender(
                    "sess-same",
                    Some("main"),
                    "own lane",
                    agent_sender("planner", Some("main")),
                ),
                message_row_with_sender(
                    "sess-cross",
                    Some("main"),
                    "cross lane",
                    agent_sender("reviewer", Some("docs")),
                ),
            ],
            LaneScope::All,
            None,
        );

        assert!(output.contains("@planner"));
        assert!(!output.contains("@planner#main"));
        assert!(output.contains("@reviewer#docs"));
    }

    #[test]
    fn retain_conversation_keeps_human_and_agent_rows() {
        let mut rows: Vec<_> = [
            MessageSender::Human,
            agent_sender("planner", None),
            MessageSender::System,
            MessageSender::Harness {
                notice: rimz::store::message::HarnessNotice::Wait,
            },
            MessageSender::Subagent {
                kind: AgentKind::new_unchecked("codex"),
                name: "child".to_owned(),
            },
        ]
        .into_iter()
        .map(|sender| message_row_with_sender("sess-coder", None, "task", sender))
        .collect();
        assert_eq!(retain_conversation(&mut rows), 3);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].sender, MessageSender::Human);
        assert_eq!(rows[1].sender, agent_sender("planner", None));
    }

    #[test]
    fn message_digest_reports_hidden_system_rows() {
        for rows in [Vec::new(), vec![message_row("sess-coder", None, "task")]] {
            let mut out = Vec::new();
            render_message_digest(
                &mut out,
                rows,
                &[],
                &LaneScope::Root,
                None,
                0,
                2,
                &empty_message_digest(&LaneScope::Root, None),
            )
            .unwrap();
            let output = String::from_utf8(out).unwrap();
            assert!(output.ends_with("... 2 system messages hidden (--system shows them)\n"));
        }
        assert!(
            !render_digest(Vec::new(), LaneScope::Root, None).contains("system messages hidden")
        );
    }

    #[test]
    fn message_digest_empty_state_describes_scope_and_status() {
        let all = render_digest(Vec::new(), LaneScope::All, None);
        assert!(all.contains("no messages"));
        assert!(!all.contains("shows every channel"));

        let main = render_digest(Vec::new(), LaneScope::Root, None);
        assert!(main.contains("no messages in #main"));
        assert!(main.contains("rimz message list --all shows every channel"));

        let named = render_digest(
            Vec::new(),
            LaneScope::Named("ops".to_owned()),
            Some(MessageStatus::Queued),
        );
        assert!(named.contains("no queued messages in #ops"));
        assert!(named.contains("rimz message list --all shows every channel"));
    }

    #[test]
    fn collapse_home_in_snippet_handles_mid_text_home_and_no_home() {
        assert_eq!(
            collapse_home_in_snippet_to(
                Some("/home/dev"),
                "see /home/dev/worktree/plan.md then /tmp"
            ),
            "see ~/worktree/plan.md then /tmp"
        );
        assert_eq!(
            collapse_home_in_snippet_to(None, "see /home/dev/worktree"),
            "see /home/dev/worktree"
        );
        assert_eq!(
            collapse_home_in_snippet_to(Some("/home/dev"), "see /home/development/plan.md"),
            "see /home/development/plan.md"
        );
        assert_eq!(
            collapse_home_in_snippet_to(Some("/home/dev/"), "see /home/dev/worktree"),
            "see ~/worktree"
        );
        assert_eq!(collapse_home_in_snippet_to(Some("/"), "/tmp"), "/tmp");
        assert_eq!(collapse_home_in_snippet_to(Some(""), "/tmp"), "/tmp");
    }

    #[test]
    fn system_sender_renders_like_rimz_attribution() {
        assert_eq!(
            rendered_sender(&MessageSender::System, "rimz"),
            rendered_sender(&agent_sender("rimz", None), "rimz")
        );
    }

    #[test]
    fn other_lanes_line_counts_only_queued_as_undelivered() {
        let mut claimed = message_row("sess-a", Some("ops"), "claimed");
        claimed.status = MessageStatus::Claimed;
        let queued = message_row("sess-b", Some("ops"), "queued");
        assert_eq!(
            other_lanes_line(
                &[claimed.clone(), queued],
                &LaneScope::Root,
                Some("project"),
                false
            )
            .as_deref(),
            Some(
                "no messages in #main. 2 in 1 other lane, 1 not yet delivered — rimz message list --all --status queued"
            )
        );
        assert_eq!(
            other_lanes_line(&[claimed], &LaneScope::Root, Some("project"), false).as_deref(),
            Some("no messages in #main. 1 in 1 other lane — rimz message list --all")
        );
        assert_eq!(
            other_lanes_line(
                &[
                    message_row("legacy", None, "legacy root"),
                    message_row("current", Some("project"), "current root"),
                ],
                &LaneScope::Named("ops".to_owned()),
                Some("project"),
                false,
            )
            .as_deref(),
            Some(
                "no messages in #ops. 2 in 1 other lane, 2 not yet delivered — rimz message list --all --status queued"
            )
        );
    }

    #[test]
    fn known_channels_line_caps_at_twenty_names() {
        let known = (0..23)
            .map(|n| format!("lane-{n:02}"))
            .collect::<std::collections::BTreeSet<_>>();
        let line = known_channels_line(&known);
        assert!(line.starts_with("lane-00, lane-01, "), "{line}");
        assert!(line.ends_with("lane-19, …and 3 more"), "{line}");
        assert_eq!(
            known_channels_line(&std::collections::BTreeSet::new()),
            "(none)"
        );
    }

    fn workspace_id() -> WorkspaceId {
        WorkspaceId::parse("ws_000000000000000000000000").unwrap()
    }

    fn render_digest(
        messages: Vec<MessageListRow>,
        lane_scope: LaneScope,
        status: Option<MessageStatus>,
    ) -> String {
        let mut out = Vec::new();
        render_message_digest(
            &mut out,
            messages,
            &[],
            &lane_scope,
            Some("project"),
            0,
            0,
            &empty_message_digest(&lane_scope, status),
        )
        .unwrap();
        String::from_utf8(out).unwrap()
    }

    fn message_row(id: &str, channel: Option<&str>, text: &str) -> MessageListRow {
        message_row_with_sender(id, channel, text, MessageSender::Human)
    }

    fn message_row_with_sender(
        id: &str,
        channel: Option<&str>,
        text: &str,
        sender: MessageSender,
    ) -> MessageListRow {
        let message = MessageRecord::new(
            workspace_id(),
            &agent(id, AgentStatus::Idle),
            text.to_owned(),
            DeliveryGate::Done,
        )
        .with_channel(channel.map(ToOwned::to_owned))
        .with_sender(sender);
        MessageListRow::from_record(message)
    }

    fn agent_sender(role: &str, channel: Option<&str>) -> MessageSender {
        MessageSender::Agent {
            agent_id: None,
            kind: AgentKind::new_unchecked("codex"),
            name: None,
            profile: None,
            role: Some(role.to_owned()),
            channel: channel.map(ToOwned::to_owned),
        }
    }

    fn agent(id: &str, status: AgentStatus) -> AgentState {
        let mut agent = AgentState::stub("claude", id, status);
        agent.pane = Some(PaneRef::from_id(PaneId::from_parts(
            MuxName::Zellij,
            "terminal_3",
        )));
        agent.worktree_path = Some("/repo/project".to_owned());
        agent.worktree_branch = Some("project".to_owned());
        agent
    }

    fn now() -> jiff::Timestamp {
        jiff::Timestamp::UNIX_EPOCH
    }
}
