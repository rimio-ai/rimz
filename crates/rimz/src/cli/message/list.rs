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
    about_run: Option<rimz::ids::RunId>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) register_by: Option<Timestamp>,
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
    fn same_notice(&self, other: &Self) -> bool {
        !self.sender.is_conversation()
            && self.sender == other.sender
            && rimz::agents::AgentCardRef::new(
                &self.kind,
                &self.agent_id,
                self.agent_name.as_deref(),
            )
            .matches(rimz::agents::AgentCardRef::new(
                &other.kind,
                &other.agent_id,
                other.agent_name.as_deref(),
            ))
            && self.body == other.body
            && self.status == other.status
    }

    pub(super) fn from_record(message: MessageRecord) -> Self {
        Self {
            message_id: message.message_id,
            about_run: message.about_run,
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
            register_by: message.register_by,
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
            about_run: None,
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
            register_by: None,
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
    if args.json {
        if limit != 0 {
            messages.truncate(limit);
        }
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
            limit,
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

struct CollapsedMessageRow<'a> {
    latest: &'a MessageListRow,
    oldest: Timestamp,
    count: usize,
}

fn collapsed_message_rows<'a>(
    messages: &'a [MessageListRow],
    root_channel: Option<&str>,
) -> Vec<CollapsedMessageRow<'a>> {
    let mut rows = Vec::<CollapsedMessageRow<'_>>::new();
    let mut last_by_lane =
        std::collections::BTreeMap::<Option<&str>, (usize, &MessageListRow)>::new();
    for message in messages {
        let lane = rimz::address::record_lane_label(message.channel.as_deref(), root_channel);
        if let Some(&(index, previous)) = last_by_lane.get(&lane)
            && previous.same_notice(message)
        {
            let row = &mut rows[index];
            row.count += 1;
            row.oldest = row.oldest.min(message.enqueued_at);
            last_by_lane.insert(lane, (index, message));
            continue;
        }
        last_by_lane.insert(lane, (rows.len(), message));
        rows.push(CollapsedMessageRow {
            latest: message,
            oldest: message.enqueued_at,
            count: 1,
        });
    }
    rows
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_message_digest(
    out: &mut impl Write,
    messages: Vec<MessageListRow>,
    agents: &[&AgentState],
    lane_scope: &LaneScope,
    root_channel: Option<&str>,
    limit: usize,
    system_hidden: usize,
    empty: &str,
) -> Result<()> {
    let now = Timestamp::now();
    let mut rows = collapsed_message_rows(&messages, root_channel);
    if limit != 0 {
        rows.truncate(limit);
    }
    let hidden = messages.len() - rows.iter().map(|row| row.count).sum::<usize>();
    if rows.is_empty() {
        writeln!(out, "{}", render::paint(render::palette::faint(), empty))?;
    } else if matches!(lane_scope, LaneScope::All) {
        for (index, (channel, rows)) in message_digest_groups(rows, root_channel)
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
        render_message_rows(out, rows, agents, root_channel, now, 0, 2)?;
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

fn render_message_rows(
    out: &mut impl Write,
    rows: Vec<CollapsedMessageRow<'_>>,
    agents: &[&AgentState],
    root_channel: Option<&str>,
    now: Timestamp,
    row_indent: usize,
    snippet_indent: usize,
) -> Result<()> {
    let row_pad = " ".repeat(row_indent);
    let snippet_pad = " ".repeat(snippet_indent);
    let snippet_width = render::terminal_columns(120).saturating_sub(snippet_indent);
    for row in rows {
        let message = row.latest;
        let lane = rimz::address::record_lane_label(message.channel.as_deref(), root_channel);
        let target = scoped_handle(message_target(message, agents, root_channel), lane);
        let sender = scoped_handle(message_sender(&message.sender, agents, root_channel), lane);
        let status = if row.count > 1 {
            format!("{} ×{}", message.status.as_str(), row.count)
        } else {
            message.status.as_str().to_owned()
        };
        writeln!(
            out,
            "{row_pad}{}{}{}  {}  {}  {}",
            rendered_sender(&message.sender, &sender),
            render::paint(render::palette::faint(), " → "),
            render::paint(render::palette::meta().bold(), &target),
            render::paint(render::status::message(message.status), &status),
            render::rel_age(message.enqueued_at, now),
            render::paint(render::palette::faint(), message.message_id.as_str())
        )?;
        let span = (row.count > 1)
            .then(|| render::folded_span(row.oldest, message.enqueued_at))
            .flatten();
        let prefix = span.map(|span| format!("{span} · ")).unwrap_or_default();
        writeln!(
            out,
            "{snippet_pad}{}",
            message_snippet(message, snippet_width, &prefix)
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

fn message_digest_groups<'a>(
    messages: Vec<CollapsedMessageRow<'a>>,
    root_channel: Option<&str>,
) -> Vec<(Option<String>, Vec<CollapsedMessageRow<'a>>)> {
    let mut groups: Vec<(Option<String>, Vec<CollapsedMessageRow<'_>>)> = Vec::new();
    for message in messages {
        let lane =
            rimz::address::record_lane_label(message.latest.channel.as_deref(), root_channel)
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
    rimz::address::agent_sender_handle(&sender, agents, None, root_channel)
        .unwrap_or_else(|| sender.render())
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

pub(super) fn message_snippet(message: &MessageListRow, width: usize, prefix: &str) -> String {
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
    let text = message.text.as_deref().or_else(|| {
        message
            .last_error
            .as_deref()
            .filter(|reason| !reason.is_empty())
    });
    let detail = text.map(collapse_home_in_snippet).unwrap_or_else(|| {
        if marker.is_some() {
            String::new()
        } else {
            "(text no longer kept)".to_owned()
        }
    });
    let suffix = marker.map_or_else(String::new, |marker| {
        if text.is_some() {
            format!(" · {marker}")
        } else {
            marker
        }
    });
    let protected_width = prefix.width() + suffix.width();
    let snippet = if text.is_none() && prefix.is_empty() && suffix.is_empty() {
        detail
    } else if protected_width <= width {
        format!(
            "{prefix}{}{suffix}",
            preview(&detail, width - protected_width)
        )
    } else {
        preview(&format!("{prefix}{detail}{suffix}"), width)
    };
    if message.text.is_some() {
        snippet
    } else {
        render::paint(render::palette::faint(), &snippet)
    }
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
mod tests;
