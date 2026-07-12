use super::ask_card::write_ask_card;
use super::scope::entry_key;
use super::thread::DisplayEntry;
use super::*;

pub(super) fn render_entry_for_log_entry(
    entry: &TranscriptEntry,
    identities: &HashMap<AgentKey, Identity>,
    include_channel: bool,
) -> RenderEntry {
    RenderEntry {
        kind: entry.entry,
        agent: entry_key(entry),
        chat: chat_entry_for_log_entry(entry, identities, include_channel),
    }
}

pub(super) fn chat_entry_for_log_entry(
    entry: &TranscriptEntry,
    identities: &HashMap<AgentKey, Identity>,
    include_channel: bool,
) -> ChatLine {
    let receiver = handle_for(entry, identities, include_channel);
    let message_id = entry.message_id.as_ref().map(ToString::to_string);
    let reply_to = entry.reply_to.iter().map(ToString::to_string).collect();
    match entry.entry {
        TranscriptKind::Prompt => ChatLine {
            from: "user".to_owned(),
            to: Some(receiver),
            at: Some(entry.at),
            text: entry.text.clone(),
            message_id,
            reply_to,
            error: false,
            questions: entry.questions.clone(),
            answers: entry.answers.clone(),
        },
        TranscriptKind::Message => ChatLine {
            from: entry.from.clone().unwrap_or_else(|| "user".to_owned()),
            to: Some(receiver),
            at: Some(entry.at),
            text: entry.text.clone(),
            message_id,
            reply_to,
            error: false,
            questions: entry.questions.clone(),
            answers: entry.answers.clone(),
        },
        TranscriptKind::Assistant | TranscriptKind::Ask => ChatLine {
            from: receiver,
            to: None,
            at: Some(entry.at),
            text: entry.text.clone(),
            message_id,
            reply_to,
            error: false,
            questions: entry.questions.clone(),
            answers: entry.answers.clone(),
        },
        TranscriptKind::Error => ChatLine {
            from: receiver,
            to: None,
            at: Some(entry.at),
            text: entry.text.clone(),
            message_id,
            reply_to,
            error: true,
            questions: Vec::new(),
            answers: Vec::new(),
        },
        TranscriptKind::Answer => ChatLine {
            from: entry.from.clone().unwrap_or_else(|| "answered".to_owned()),
            to: Some(receiver),
            at: Some(entry.at),
            text: entry.text.clone(),
            message_id,
            reply_to,
            error: false,
            questions: entry.questions.clone(),
            answers: entry.answers.clone(),
        },
    }
}

pub(super) fn handle_for(
    entry: &TranscriptEntry,
    identities: &HashMap<AgentKey, Identity>,
    include_channel: bool,
) -> String {
    let key = entry_key(entry);
    if let Some(identity) = identities.get(&key) {
        return render_handle(
            &identity.base_handle,
            entry.channel.as_deref().or(identity.channel.as_deref()),
            include_channel,
        );
    }
    let base = rimz::message::identity_handle(&entry.kind, None, None);
    render_handle(&base, entry.channel.as_deref(), include_channel)
}

pub(super) fn render_handle(base: &str, channel: Option<&str>, include_channel: bool) -> String {
    if include_channel && let Some(channel) = channel.filter(|channel| !channel.is_empty()) {
        return format!("{base}#{channel}");
    }
    base.to_owned()
}

pub(super) const GROUP_WINDOW_SECS: i64 = 5 * 60;

#[derive(Default)]
pub(super) struct BrandColors {
    by_handle: HashMap<String, AgentKind>,
}

impl BrandColors {
    fn insert(&mut self, handle: &str, kind: AgentKind) {
        self.by_handle
            .entry(base_handle(handle).to_owned())
            .or_insert(kind);
    }

    fn style_for(&self, handle: &str) -> anstyle::Style {
        self.by_handle
            .get(base_handle(handle))
            .and_then(|kind| rimz::agents::registry::descriptor_by_kind(kind.as_str()))
            .map(|descriptor| render::palette::rgb(descriptor.brand.color_rgb).bold())
            .unwrap_or(render::palette::META.bold())
    }
}

pub(super) fn base_handle(handle: &str) -> &str {
    // In rendered agent handles, `#` only separates the channel suffix.
    handle.split_once('#').map_or(handle, |(base, _)| base)
}

pub(super) fn write_header(out: &mut impl Write, channel: Option<&str>) -> Result<()> {
    if let Some(channel) = channel {
        writeln!(
            out,
            "{}",
            render::paint(render::palette::COOL.bold(), &format!("#{channel}"))
        )?;
        writeln!(out)?;
    }
    Ok(())
}

pub(super) fn display_handle(handle: &str, grouped: bool) -> &str {
    if grouped { base_handle(handle) } else { handle }
}

#[cfg(test)]
pub(super) fn render_chat_to(
    out: &mut impl Write,
    channel: Option<&str>,
    entries: &[RenderEntry],
    archive_prefix: usize,
    tz: &TimeZone,
    today: Date,
) -> Result<()> {
    render_display_chat_to(
        out,
        channel,
        &super::thread::flat_entries(entries, archive_prefix),
        tz,
        today,
    )
}

pub(super) fn render_display_chat_to(
    out: &mut impl Write,
    channel: Option<&str>,
    entries: &[DisplayEntry],
    tz: &TimeZone,
    today: Date,
) -> Result<()> {
    write_header(out, channel)?;
    let grouped = channel.is_some();
    let rendered = entries
        .iter()
        .map(|display| display.entry.clone())
        .collect::<Vec<_>>();
    let folded = pair_answers(&rendered);
    let mut brands = BrandColors::default();
    for display in entries {
        let entry = &display.entry;
        let handle = match entry.kind {
            TranscriptKind::Assistant | TranscriptKind::Ask | TranscriptKind::Error => {
                entry.chat.from.as_str()
            }
            _ => entry.chat.to.as_deref().unwrap_or(entry.chat.from.as_str()),
        };
        brands.insert(handle, entry.agent.0.clone());
    }
    let mut last_date = Some(today);
    let mut first_entry = true;
    let mut follows_day_delimiter = false;
    let mut last_group: Option<GroupState> = None;
    let mut previous_block = None;
    let newest_archived_at = entries
        .iter()
        .filter(|entry| entry.archived)
        .filter_map(|entry| entry.entry.chat.at)
        .max();
    let mut wrote_live_divider = newest_archived_at.is_none();
    for (index, display) in entries.iter().enumerate() {
        if folded.suppressed_answers.contains(&index) {
            continue;
        }
        let entry = &display.entry;
        let entry_date = entry.chat.at.map(|at| at.to_zoned(tz.clone()).date());
        if display.lane.is_margin() && !wrote_live_divider && !display.archived {
            if let Some(at) = entry.chat.at {
                write_live_divider(out, at, tz, today)?;
            }
            last_date = entry_date;
            last_group = None;
            follows_day_delimiter = true;
            wrote_live_divider = true;
        }
        if display.lane.is_margin()
            && let Some(date) = entry_date
            && Some(date) != last_date
        {
            write_day_delimiter(out, date, today)?;
            last_date = Some(date);
            follows_day_delimiter = true;
            last_group = None;
        }
        let is_ask = entry.kind == TranscriptKind::Ask;
        let continuation = !is_ask
            && last_group
                .as_ref()
                .is_some_and(|group| group.matches(display, grouped, entry_date));
        if !continuation && !first_entry && !follows_day_delimiter {
            if previous_block == Some(display.block) && !display.lane.is_margin() {
                writeln!(out, "{}", render::paint(render::palette::FAINT, "│"))?;
            } else {
                writeln!(out)?;
            }
        }
        let answer = folded
            .answer_by_ask
            .get(&index)
            .map(|answer| &entries[*answer].entry);
        if display.lane.is_margin() {
            write_entry_content(
                out,
                entry,
                answer,
                continuation,
                grouped,
                &brands,
                tz,
                false,
            )?;
        } else {
            let mut buffer = Vec::new();
            let show_date = display
                .lane
                .root_at()
                .zip(entry.chat.at)
                .is_some_and(|(root, at)| {
                    root.to_zoned(tz.clone()).date() != at.to_zoned(tz.clone()).date()
                });
            write_entry_content(
                &mut buffer,
                entry,
                answer,
                continuation,
                grouped,
                &brands,
                tz,
                show_date,
            )?;
            write_thread_lines(out, &buffer)?;
        }
        if is_ask {
            last_group = None;
        } else {
            last_group = Some(GroupState::new(display, grouped, entry_date));
        }
        first_entry = false;
        follows_day_delimiter = false;
        previous_block = Some(display.block);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn write_entry_content(
    out: &mut impl Write,
    entry: &RenderEntry,
    answer: Option<&RenderEntry>,
    continuation: bool,
    grouped: bool,
    brands: &BrandColors,
    tz: &TimeZone,
    show_date: bool,
) -> Result<()> {
    if !continuation {
        write_entry_header(out, entry, grouped, brands, tz, show_date)?;
    }
    if entry.kind == TranscriptKind::Ask {
        write_ask_card(out, entry, answer)
    } else if entry.chat.error {
        write_body_lines_with(out, &entry.chat.text, Some(render::palette::ALARM))
    } else {
        write_body_lines(out, &entry.chat.text)
    }
}

fn write_thread_lines(out: &mut impl Write, rendered: &[u8]) -> Result<()> {
    let rendered = std::str::from_utf8(rendered).expect("transcript rendering is utf-8");
    for line in rendered.split_terminator('\n') {
        let spine = render::paint(render::palette::FAINT, "│");
        if line.is_empty() {
            writeln!(out, "{spine}")?;
        } else {
            writeln!(out, "{spine} {line}")?;
        }
    }
    Ok(())
}

#[derive(Default)]
pub(super) struct AnswerPairs {
    answer_by_ask: HashMap<usize, usize>,
    suppressed_answers: BTreeSet<usize>,
}

pub(super) fn pair_answers(entries: &[RenderEntry]) -> AnswerPairs {
    let mut folded = AnswerPairs::default();
    let mut open_by_agent: HashMap<AgentKey, Vec<usize>> = HashMap::new();
    for (index, entry) in entries.iter().enumerate() {
        match entry.kind {
            TranscriptKind::Ask => {
                open_by_agent
                    .entry(entry.agent.clone())
                    .or_default()
                    .push(index);
            }
            TranscriptKind::Answer => {
                let mut by_agent = || {
                    let stack = open_by_agent.get_mut(&entry.agent)?;
                    while let Some(ask) = stack.pop() {
                        if !folded.answer_by_ask.contains_key(&ask) {
                            return Some(ask);
                        }
                    }
                    None
                };
                let ask = by_agent();
                if let Some(ask) = ask {
                    folded.answer_by_ask.insert(ask, index);
                    folded.suppressed_answers.insert(index);
                }
            }
            _ => {}
        }
    }
    folded
}

#[derive(Clone, Debug)]
pub(super) struct GroupState {
    from: String,
    to: Option<String>,
    at: Option<jiff::Timestamp>,
    date: Option<Date>,
    lane: Option<usize>,
}

impl GroupState {
    fn new(entry: &DisplayEntry, grouped: bool, date: Option<Date>) -> Self {
        let lane = entry.lane.group_key();
        let (from, to) = group_key(&entry.entry, grouped);
        Self {
            from,
            to,
            at: entry.entry.chat.at,
            date,
            lane,
        }
    }

    fn matches(&self, entry: &DisplayEntry, grouped: bool, date: Option<Date>) -> bool {
        if self.lane != entry.lane.group_key() {
            return false;
        }
        let (from, to) = group_key(&entry.entry, grouped);
        if self.from != from || self.to != to || self.date != date {
            return false;
        }
        let (Some(previous), Some(current)) = (self.at, entry.entry.chat.at) else {
            return false;
        };
        let gap = current.duration_since(previous);
        !gap.is_negative() && gap.as_secs() <= GROUP_WINDOW_SECS
    }
}

pub(super) fn group_key(entry: &RenderEntry, grouped: bool) -> (String, Option<String>) {
    (
        display_handle(&entry.chat.from, grouped).to_owned(),
        entry
            .chat
            .to
            .as_deref()
            .map(|to| display_handle(to, grouped).to_owned()),
    )
}

pub(super) fn write_entry_header(
    out: &mut impl Write,
    entry: &RenderEntry,
    grouped: bool,
    brands: &BrandColors,
    tz: &TimeZone,
    show_date: bool,
) -> Result<()> {
    let mut header = paint_handle(&entry.chat.from, grouped, brands);
    if let Some(to) = entry.chat.to.as_deref() {
        header.push_str(&render::paint(render::palette::FAINT, " → "));
        header.push_str(&paint_handle(to, grouped, brands));
    }
    if let Some(at) = entry.chat.at {
        header.push_str("  ");
        let format = if show_date {
            "%a, %b %-d %Y · %H:%M"
        } else {
            "%H:%M"
        };
        header.push_str(&render::paint(
            render::palette::FAINT,
            &at.to_zoned(tz.clone()).strftime(format).to_string(),
        ));
    }
    writeln!(out, "{header}")?;
    Ok(())
}

pub(super) fn paint_handle(handle: &str, grouped: bool, brands: &BrandColors) -> String {
    match base_handle(handle) {
        label @ ("user" | "you" | "answered") => chip(render::palette::HUMAN_CHIP, label),
        "rimz" => chip(render::palette::SYSTEM_CHIP, "rimz"),
        _ => render::paint(brands.style_for(handle), display_handle(handle, grouped)),
    }
}

fn chip(style: anstyle::Style, label: &str) -> String {
    render::paint(style, &format!(" {label} "))
}

pub(super) fn write_body_lines(out: &mut impl Write, text: &str) -> Result<()> {
    write_body_lines_with(out, text, None)
}

pub(super) fn write_body_lines_with(
    out: &mut impl Write,
    text: &str,
    style: Option<anstyle::Style>,
) -> Result<()> {
    for line in text.lines() {
        if line.is_empty() {
            writeln!(out)?;
        } else {
            writeln!(out, "{}", paint_mentions_with(line, style))?;
        }
    }
    Ok(())
}

pub(super) fn paint_mentions_with(line: &str, base_style: Option<anstyle::Style>) -> String {
    let mut rendered = String::new();
    let mut index = 0;
    while index < line.len() {
        let ch = line[index..]
            .chars()
            .next()
            .expect("index stays on char boundary");
        if matches!(ch, '@' | '#') && mention_boundary(line, index) {
            let token_start = index + ch.len_utf8();
            let mut token_end = token_start;
            for (offset, token_ch) in line[token_start..].char_indices() {
                if is_mention_char(token_ch) {
                    token_end = token_start + offset + token_ch.len_utf8();
                } else {
                    break;
                }
            }
            let mut paint_end = token_end;
            while paint_end > token_start {
                let tail = line[..paint_end]
                    .chars()
                    .next_back()
                    .expect("paint_end stays on char boundary");
                if matches!(tail, '.' | ',' | ';' | ':' | '!' | '?' | ')') {
                    paint_end -= tail.len_utf8();
                } else {
                    break;
                }
            }
            if paint_end > token_start {
                push_painted(&mut rendered, base_style, &line[..index]);
                rendered.push_str(&render::paint(
                    render::palette::COOL.bold(),
                    &line[index..paint_end],
                ));
                push_painted(&mut rendered, base_style, &line[paint_end..token_end]);
                let rest = &line[token_end..];
                rendered.push_str(&paint_mentions_with(rest, base_style));
                return rendered;
            }
        }
        index += ch.len_utf8();
    }
    push_painted(&mut rendered, base_style, line);
    rendered
}

pub(super) fn push_painted(rendered: &mut String, style: Option<anstyle::Style>, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(style) = style {
        rendered.push_str(&render::paint(style, text));
    } else {
        rendered.push_str(text);
    }
}

pub(super) fn mention_boundary(line: &str, index: usize) -> bool {
    if index == 0 {
        return true;
    }
    line[..index]
        .chars()
        .next_back()
        .is_some_and(|ch| ch.is_whitespace() || ch == '(')
}

pub(super) fn is_mention_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '/' | '-')
}

pub(super) fn write_day_delimiter(out: &mut impl Write, date: Date, today: Date) -> Result<()> {
    let label = if date == today {
        "Today".to_owned()
    } else {
        date.strftime("%a, %b %-d %Y").to_string()
    };
    write_faint_rule(out, &label)
}

pub(super) fn write_live_divider(
    out: &mut impl Write,
    at: jiff::Timestamp,
    tz: &TimeZone,
    today: Date,
) -> Result<()> {
    write_faint_rule(
        out,
        &format!("Live · {}", format_marker_when(at, tz, today)),
    )
}

pub(super) fn format_marker_when(at: jiff::Timestamp, tz: &TimeZone, today: Date) -> String {
    let zoned = at.to_zoned(tz.clone());
    let date = zoned.date();
    if date == today {
        format!("earlier today · {}", zoned.strftime("%H:%M"))
    } else {
        date.strftime("%a, %b %-d %Y").to_string()
    }
}

fn write_faint_rule(out: &mut impl Write, label: &str) -> Result<()> {
    let width = render::terminal_columns(48).min(48);
    let label = format!("  {label}  ");
    let dashes = width.saturating_sub(label.chars().count());
    let (left, right) = (dashes / 2, dashes - dashes / 2);
    let rule = format!("{}{label}{}", "─".repeat(left), "─".repeat(right));
    writeln!(out)?;
    writeln!(out, "{}", render::paint(render::palette::RULE, &rule))?;
    writeln!(out)?;
    Ok(())
}
