//! `rimz transcript` — inspect agent and channel conversations from local logs.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;

use anyhow::{Context, Result, bail};
use clap::Args;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde::Serialize;

use super::{GlobalFlags, current_channel};
use crate::cli::render;
use rimz::agents::transcript::{self, AgentChat};
use rimz::feed::FeedItem;
use rimz::ids::{AgentKind, AgentSessionId, RequestId};
use rimz::ledger::transcript_log::{TranscriptEntry, TranscriptKind};
use rimz::workspace::WorkspaceResolver;

#[derive(Debug, Args)]
pub struct TranscriptArgs {
    /// Agent address, `#channel`, or `@all`. Omit for the current channel.
    target: Option<String>,
    /// Override the channel/worktree used to resolve the target.
    #[arg(short = 'w', long)]
    worktree: Option<String>,
    /// Keep the last N chat lines.
    #[arg(short = 'n', long)]
    last: Option<usize>,
    /// Render every normalized message instead of turn summaries.
    #[arg(long)]
    details: bool,
    /// Emit JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Serialize)]
pub(crate) struct AskView {
    pub request_id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub options: Vec<String>,
    pub surface: rimz::Surface,
}

#[derive(Serialize)]
struct ChatView {
    #[serde(skip_serializing_if = "Option::is_none")]
    channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    focus: Option<String>,
    entries: Vec<rimz::agents::ChatEntry>,
}

type AgentKey = (AgentKind, AgentSessionId);

#[derive(Clone, Debug)]
struct Identity {
    base_handle: String,
    channel: Option<String>,
    name: Option<String>,
    profile: Option<String>,
    role: Option<String>,
    rich: bool,
}

#[derive(Clone, Debug)]
struct Scope {
    channel: Option<String>,
    channel_filter: Option<String>,
    focus: Option<String>,
    focus_keys: Option<BTreeSet<AgentKey>>,
    include_channel: bool,
}

pub fn run(args: TranscriptArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let paths = rimz::StatePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing state paths")?;
    let current = current_channel(&workspace);
    let entries = dedup_asks(rimz::ledger::transcript_log::read_all(&paths)?);
    if entries.is_empty() {
        bail!("no transcripts on disk yet");
    }
    let identities = build_identities(&entries);
    let scope = resolve_scope(
        args.target.as_deref(),
        args.worktree.as_deref(),
        current.as_deref(),
        &identities,
    )?;
    let filtered: Vec<&TranscriptEntry> = entries
        .iter()
        .filter(|entry| channel_matches(entry.channel.as_deref(), scope.channel_filter.as_deref()))
        .collect();
    if filtered.is_empty() {
        bail!("no transcripts on disk for this scope yet");
    }

    let mut per_agent_messages: BTreeMap<AgentKey, Vec<rimz::agents::TranscriptMessage>> =
        BTreeMap::new();
    let mut direct = Vec::new();
    for entry in filtered {
        let key = entry_key(entry);
        match entry.entry {
            TranscriptKind::Prompt | TranscriptKind::Assistant => {
                per_agent_messages
                    .entry(key)
                    .or_default()
                    .push(rimz::agents::TranscriptMessage {
                        role: match entry.entry {
                            TranscriptKind::Prompt => rimz::agents::TranscriptRole::User,
                            TranscriptKind::Assistant => rimz::agents::TranscriptRole::Assistant,
                            TranscriptKind::Ask | TranscriptKind::Answer => unreachable!(),
                        },
                        at: Some(entry.at),
                        text: entry.text.clone(),
                    });
            }
            TranscriptKind::Ask => direct.push((
                key,
                rimz::agents::ChatEntry {
                    from: handle_for(entry, &identities, scope.include_channel),
                    to: None,
                    at: Some(entry.at),
                    text: entry.text.clone(),
                },
            )),
            TranscriptKind::Answer => direct.push((
                key,
                rimz::agents::ChatEntry {
                    from: entry.from.clone().unwrap_or_else(|| "resolver".to_owned()),
                    to: Some(handle_for(entry, &identities, scope.include_channel)),
                    at: Some(entry.at),
                    text: entry.text.clone(),
                },
            )),
        }
    }

    let mut entries = Vec::new();
    for (key, mut messages) in per_agent_messages {
        sort_transcript_messages(&mut messages);
        let mut chat = transcript::build_chat(
            vec![AgentChat {
                handle: handle_for_key(&key, &identities, scope.include_channel),
                messages,
            }],
            args.details,
            rimz::target::parse_sender_prefix,
        );
        if let Some(focus) = scope.focus_keys.as_ref() {
            chat.retain(|entry| {
                focus.contains(&key)
                    || sender_matches_focus(
                        &entry.from,
                        focus,
                        &identities,
                        scope.channel_filter.as_deref(),
                    )
            });
        }
        entries.extend(chat);
    }
    entries.extend(direct.into_iter().filter_map(|(key, entry)| {
        scope
            .focus_keys
            .as_ref()
            .is_none_or(|focus| focus.contains(&key))
            .then_some(entry)
    }));
    entries.sort_by(|left, right| compare_optional_timestamps(left.at, right.at));

    if entries.is_empty() {
        bail!("no transcripts on disk for this scope yet");
    }
    keep_last(&mut entries, args.last);

    if args.json {
        print_json(&ChatView {
            channel: scope.channel,
            focus: scope.focus,
            entries,
        })?;
    } else {
        let tz = super::machine_config().time_zone();
        render_chat(scope.channel.as_deref(), &entries, &tz)?;
    }
    Ok(())
}

fn sort_transcript_messages(messages: &mut [rimz::agents::TranscriptMessage]) {
    messages.sort_by(|left, right| compare_optional_timestamps(left.at, right.at));
}

fn compare_optional_timestamps(
    left: Option<jiff::Timestamp>,
    right: Option<jiff::Timestamp>,
) -> std::cmp::Ordering {
    match (left, right) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn sender_matches_focus(
    sender: &str,
    focus: &BTreeSet<AgentKey>,
    identities: &HashMap<AgentKey, Identity>,
    channel_filter: Option<&str>,
) -> bool {
    let matches = matching_handle_keys(sender, channel_filter, identities);
    matches.len() == 1 && focus.contains(matches[0])
}

fn matching_handle_keys<'a>(
    handle: &str,
    channel_filter: Option<&str>,
    identities: &'a HashMap<AgentKey, Identity>,
) -> Vec<&'a AgentKey> {
    let (base, channel) = split_rendered_handle(handle);
    let mut matches: Vec<_> = identities
        .iter()
        .filter_map(|(key, identity)| {
            (identity.base_handle == base
                && channel_matches(identity.channel.as_deref(), channel.or(channel_filter)))
            .then_some(key)
        })
        .collect();
    matches.sort();
    matches
}

fn split_rendered_handle(handle: &str) -> (&str, Option<&str>) {
    handle
        .split_once('#')
        .map_or((handle, None), |(base, channel)| (base, Some(channel)))
}

fn dedup_asks(entries: Vec<TranscriptEntry>) -> Vec<TranscriptEntry> {
    let mut latest_asks: HashMap<RequestId, (usize, jiff::Timestamp)> = HashMap::new();
    for (index, entry) in entries.iter().enumerate() {
        if entry.entry == TranscriptKind::Ask
            && let Some(request_id) = entry.request_id.as_ref()
        {
            latest_asks
                .entry(request_id.clone())
                .and_modify(|prior| {
                    if entry.at >= prior.1 {
                        *prior = (index, entry.at);
                    }
                })
                .or_insert((index, entry.at));
        }
    }
    entries
        .into_iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            if entry.entry == TranscriptKind::Ask
                && let Some(request_id) = entry.request_id.as_ref()
            {
                return (latest_asks.get(request_id).map(|(latest, _)| *latest) == Some(index))
                    .then_some(entry);
            }
            Some(entry)
        })
        .collect()
}

fn build_identities(entries: &[TranscriptEntry]) -> HashMap<AgentKey, Identity> {
    let mut identities = HashMap::new();
    for entry in entries {
        let candidate = Identity {
            base_handle: rimz::target::identity_handle(
                &entry.kind,
                entry.name.as_deref(),
                entry.profile.as_deref(),
                entry.role.as_deref(),
            ),
            channel: entry.channel.clone(),
            name: entry.name.clone(),
            profile: entry.profile.clone(),
            role: entry.role.clone(),
            rich: entry.role.is_some() || entry.name.is_some() || entry.profile.is_some(),
        };
        identities
            .entry(entry_key(entry))
            .and_modify(|existing: &mut Identity| {
                if existing.channel.is_none() {
                    existing.channel = candidate.channel.clone();
                }
                if candidate.rich && !existing.rich {
                    existing.base_handle = candidate.base_handle.clone();
                    existing.name = candidate.name.clone();
                    existing.profile = candidate.profile.clone();
                    existing.role = candidate.role.clone();
                    existing.rich = true;
                }
            })
            .or_insert(candidate);
    }
    identities
}

fn resolve_scope(
    target: Option<&str>,
    worktree: Option<&str>,
    current: Option<&str>,
    identities: &HashMap<AgentKey, Identity>,
) -> Result<Scope> {
    match target {
        None => {
            let channel = worktree.or(current).map(ToOwned::to_owned);
            let include_channel = channel.is_none();
            Ok(Scope {
                channel: channel.clone(),
                channel_filter: channel,
                focus: None,
                focus_keys: None,
                include_channel,
            })
        }
        Some(raw) if raw.starts_with('#') => {
            let channel = raw.trim_start_matches('#');
            if channel.is_empty() {
                bail!("channel target must be `#<name>`");
            }
            if let Some(flag) = worktree
                && flag != channel
            {
                bail!("target `{raw}` names channel `#{channel}` but --worktree names `{flag}`");
            }
            Ok(single_channel_scope(channel.to_owned()))
        }
        Some(raw) if raw == "@all" || raw.starts_with("@all#") => {
            let inline = raw.split_once('#').map(|(_, channel)| channel);
            if inline == Some("") {
                bail!("channel suffix in target `{raw}` must name a channel");
            }
            let channel = reconcile_channel(raw, inline, worktree, None)?;
            let include_channel = channel.is_none();
            Ok(Scope {
                channel: channel.clone(),
                channel_filter: channel,
                focus: None,
                focus_keys: None,
                include_channel,
            })
        }
        Some(raw) => {
            let (selector, inline) = split_agent_target(raw)?;
            let explicit_or_current = reconcile_channel(raw, inline, worktree, current)?;
            let matches = matching_identities(selector, explicit_or_current.as_deref(), identities);
            let (key, identity) = match matches.as_slice() {
                [(key, identity)] => (*key, *identity),
                [] => bail!("no agent matches target `{raw}` in the transcript log"),
                _ => return Err(ambiguous_target(raw, &matches)),
            };
            let channel = explicit_or_current.or_else(|| identity.channel.clone());
            let include_channel = channel.is_none();
            let focus = Some(render_handle(
                &identity.base_handle,
                identity.channel.as_deref(),
                include_channel,
            ));
            Ok(Scope {
                channel: channel.clone(),
                channel_filter: channel,
                focus,
                focus_keys: Some(BTreeSet::from([(*key).clone()])),
                include_channel,
            })
        }
    }
}

fn single_channel_scope(channel: String) -> Scope {
    Scope {
        channel: Some(channel.clone()),
        channel_filter: Some(channel),
        focus: None,
        focus_keys: None,
        include_channel: false,
    }
}

fn split_agent_target(raw: &str) -> Result<(&str, Option<&str>)> {
    match raw.split_once('#') {
        Some((selector, channel)) if !selector.is_empty() && !channel.is_empty() => {
            Ok((selector, Some(channel)))
        }
        Some((_, "")) => bail!("channel suffix in target `{raw}` must name a channel"),
        _ => Ok((raw, None)),
    }
}

fn reconcile_channel(
    raw: &str,
    inline: Option<&str>,
    flag: Option<&str>,
    fallback: Option<&str>,
) -> Result<Option<String>> {
    match (inline, flag) {
        (Some(channel), Some(flag)) if channel != flag => {
            bail!("target `{raw}` names channel `#{channel}` but --worktree names `{flag}`")
        }
        (Some(channel), _) => Ok(Some(channel.to_owned())),
        (None, Some(flag)) => Ok(Some(flag.to_owned())),
        (None, None) => Ok(fallback.map(ToOwned::to_owned)),
    }
}

fn matching_identities<'a>(
    selector: &str,
    channel: Option<&str>,
    identities: &'a HashMap<AgentKey, Identity>,
) -> Vec<(&'a AgentKey, &'a Identity)> {
    let selector = selector.strip_prefix('@').unwrap_or(selector);
    let wanted_handle = format!("@{selector}");
    let mut matches: Vec<_> = identities
        .iter()
        .filter(|(key, identity)| {
            (identity.base_handle == wanted_handle
                || key.0.as_str() == selector
                || identity.name.as_deref() == Some(selector)
                || identity.profile.as_deref() == Some(selector)
                || identity.role.as_deref() == Some(selector)
                || key.1.as_str() == selector
                || key.1.as_str().starts_with(selector))
                && channel_matches(identity.channel.as_deref(), channel)
        })
        .collect();
    matches.sort_by(|left, right| {
        candidate_label(left.0, left.1).cmp(&candidate_label(right.0, right.1))
    });
    matches
}

fn ambiguous_target(raw: &str, matches: &[(&AgentKey, &Identity)]) -> anyhow::Error {
    let candidates = matches
        .iter()
        .map(|(key, identity)| candidate_label(key, identity))
        .collect::<Vec<_>>()
        .join(", ");
    anyhow::anyhow!("target `{raw}` matched multiple agents in transcript log: {candidates}")
}

fn candidate_label(key: &AgentKey, identity: &Identity) -> String {
    let handle = render_handle(&identity.base_handle, identity.channel.as_deref(), true);
    format!("{handle} ({})", key.1.as_str())
}

fn channel_matches(entry_channel: Option<&str>, filter: Option<&str>) -> bool {
    filter.is_none_or(|filter| entry_channel == Some(filter))
}

fn entry_key(entry: &TranscriptEntry) -> AgentKey {
    (entry.kind.clone(), entry.agent_id.clone())
}

fn handle_for(
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
    let base = rimz::target::identity_handle(&entry.kind, None, None, None);
    render_handle(&base, entry.channel.as_deref(), include_channel)
}

fn handle_for_key(
    key: &AgentKey,
    identities: &HashMap<AgentKey, Identity>,
    include_channel: bool,
) -> String {
    let Some(identity) = identities.get(key) else {
        return rimz::target::identity_handle(&key.0, None, None, None);
    };
    render_handle(
        &identity.base_handle,
        identity.channel.as_deref(),
        include_channel,
    )
}

fn render_handle(base: &str, channel: Option<&str>, include_channel: bool) -> String {
    if include_channel && let Some(channel) = channel.filter(|channel| !channel.is_empty()) {
        return format!("{base}#{channel}");
    }
    base.to_owned()
}

fn keep_last<T>(items: &mut Vec<T>, last: Option<usize>) {
    let Some(last) = last else {
        return;
    };
    let drop = items.len().saturating_sub(last);
    if drop > 0 {
        items.drain(..drop);
    }
}

pub(crate) fn ask_view(item: &FeedItem) -> AskView {
    AskView {
        request_id: item.request_id.to_string(),
        title: item.title.clone(),
        body: item.body.clone(),
        options: item.options.clone(),
        surface: item.surface,
    }
}

pub(crate) fn ask_summary(ask: &AskView) -> String {
    rimz::feed::ask_summary(&ask.title, ask.body.as_deref(), &ask.options)
}

const AGENT_TONES: [anstyle::Style; 3] = [
    render::palette::META,
    render::palette::ACCENT,
    render::palette::GOOD,
];
const BODY_INDENT: &str = "    ";

#[derive(Default)]
struct AgentTones {
    order: Vec<String>,
}

impl AgentTones {
    fn tone(&mut self, handle: &str) -> anstyle::Style {
        let idx = self
            .order
            .iter()
            .position(|seen| seen == handle)
            .unwrap_or_else(|| {
                self.order.push(handle.to_owned());
                self.order.len() - 1
            });
        AGENT_TONES[idx % AGENT_TONES.len()]
    }
}

fn base_handle(handle: &str) -> &str {
    // In rendered agent handles, `#` only separates the channel suffix.
    handle.split_once('#').map_or(handle, |(base, _)| base)
}

fn write_header(out: &mut impl Write, channel: Option<&str>) -> Result<()> {
    if let Some(channel) = channel {
        writeln!(
            out,
            "{}",
            render::paint(render::palette::ACCENT.bold(), &format!("#{channel}"))
        )?;
        writeln!(out)?;
    }
    Ok(())
}

fn display_handle(handle: &str, grouped: bool) -> &str {
    if grouped { base_handle(handle) } else { handle }
}

fn render_chat(
    channel: Option<&str>,
    entries: &[rimz::agents::ChatEntry],
    tz: &TimeZone,
) -> Result<()> {
    let mut out = render::out();
    let today = jiff::Timestamp::now().to_zoned(tz.clone()).date();
    render_chat_to(&mut out, channel, entries, tz, today)
}

fn render_chat_to(
    out: &mut impl Write,
    channel: Option<&str>,
    entries: &[rimz::agents::ChatEntry],
    tz: &TimeZone,
    today: Date,
) -> Result<()> {
    write_header(out, channel)?;
    let grouped = channel.is_some();
    let mut tones = AgentTones::default();
    let mut last_date = Some(today);
    let mut first_entry = true;
    let mut follows_day_delimiter = false;
    for entry in entries {
        if let Some(at) = entry.at {
            let date = at.to_zoned(tz.clone()).date();
            if Some(date) != last_date {
                write_day_delimiter(out, date, today)?;
                last_date = Some(date);
                follows_day_delimiter = true;
            }
        }
        if !first_entry && !follows_day_delimiter {
            writeln!(out)?;
        }
        let from = if entry.from == "user" {
            render::paint(render::palette::COOL, "user")
        } else if entry.from == "you" {
            render::paint(render::palette::COOL.bold(), "you")
        } else {
            let display = display_handle(&entry.from, grouped);
            render::paint(tones.tone(base_handle(&entry.from)).bold(), display)
        };
        let to = entry
            .to
            .as_deref()
            .map(|to| format!("{}, ", display_handle(to, grouped)))
            .unwrap_or_default();
        write_chat_line(out, entry.at, &from, &format!("{to}{}", entry.text), tz)?;
        first_entry = false;
        follows_day_delimiter = false;
    }
    Ok(())
}

fn write_day_delimiter(out: &mut impl Write, date: Date, today: Date) -> Result<()> {
    const WIDTH: usize = 26;
    let label = if date == today {
        "Today".to_owned()
    } else {
        date.strftime("%a, %b %-d %Y").to_string()
    };
    let mut rule = format!("──── {label} ");
    while rule.chars().count() < WIDTH {
        rule.push('─');
    }
    writeln!(out, "{}", render::paint(render::palette::FAINT, &rule))?;
    Ok(())
}

fn write_chat_line(
    out: &mut impl Write,
    at: Option<jiff::Timestamp>,
    from: &str,
    text: &str,
    tz: &TimeZone,
) -> Result<()> {
    let time = at.map_or_else(
        || "        ".to_owned(),
        |at| at.to_zoned(tz.clone()).strftime("%H:%M:%S").to_string(),
    );
    let time = render::paint(render::palette::FAINT, &time);
    writeln!(out, "{time}  {from}")?;
    for line in text.lines() {
        if line.is_empty() {
            writeln!(out)?;
        } else {
            writeln!(out, "{BODY_INDENT}{line}")?;
        }
    }
    Ok(())
}

fn print_json(value: &impl Serialize) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(raw: &str) -> jiff::Timestamp {
        raw.parse().expect("timestamp")
    }

    fn entry(at: &str, text: &str) -> rimz::agents::ChatEntry {
        rimz::agents::ChatEntry {
            from: "user".to_owned(),
            to: None,
            at: Some(ts(at)),
            text: text.to_owned(),
        }
    }

    fn ask_entry(at: &str, text: &str) -> rimz::agents::ChatEntry {
        rimz::agents::ChatEntry {
            from: "claude".to_owned(),
            to: None,
            at: Some(ts(at)),
            text: text.to_owned(),
        }
    }

    fn render(entries: &[rimz::agents::ChatEntry], today: Date) -> String {
        let tz = TimeZone::get("America/New_York").expect("timezone");
        let mut out = Vec::new();
        render_chat_to(&mut out, None, entries, &tz, today).expect("render");
        String::from_utf8(out).expect("utf8")
    }

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(ch) = chars.next() {
            if ch == '\x1b' {
                for ch in chars.by_ref() {
                    if ch == 'm' {
                        break;
                    }
                }
            } else {
                out.push(ch);
            }
        }
        out
    }

    #[test]
    fn chat_renders_configured_zone_and_day_boundaries() {
        let today = jiff::civil::date(2026, 6, 28);
        let out = render(
            &[
                entry("2026-06-27T03:30:00Z", "late friday"),
                entry("2026-06-27T14:00:00Z", "saturday"),
                entry("2026-06-28T16:00:00Z", "today"),
            ],
            today,
        );

        let friday = out.find("──── Fri, Jun 26 2026 ────").expect("friday");
        let saturday = out.find("──── Sat, Jun 27 2026 ────").expect("saturday");
        let today = out.find("──── Today ").expect("today");
        assert!(friday < saturday);
        assert!(saturday < today);
        assert!(out.contains("23:30:00"));
        assert!(out.contains("10:00:00"));
        assert!(out.contains("12:00:00"));
        assert!(!out.contains("03:30:00"));
    }

    #[test]
    fn today_only_chat_omits_day_delimiter() {
        let out = render(
            &[entry("2026-06-28T04:30:00Z", "same day")],
            jiff::civil::date(2026, 6, 28),
        );

        assert!(!out.contains("────"));
        assert!(out.contains("00:30:00"));
    }

    #[test]
    fn chat_renders_speaker_headers_and_body_indent() {
        let out = render(
            &[
                entry("2026-06-28T04:00:00Z", "hello\n\nagain"),
                ask_entry("2026-06-28T04:00:01Z", "answer"),
            ],
            jiff::civil::date(2026, 6, 28),
        );
        let out = strip_ansi(&out);

        assert!(
            out.contains("00:00:00  user\n    hello\n\n    again\n\n00:00:01  claude\n    answer"),
            "{out}"
        );
        assert!(!out.contains("user:"), "{out}");
    }

    #[test]
    fn timestamped_asks_advance_day_delimiter() {
        let out = render(
            &[
                entry("2026-06-27T14:00:00Z", "yesterday"),
                ask_entry("2026-06-28T16:00:00Z", "Approve tool?"),
            ],
            jiff::civil::date(2026, 6, 28),
        );

        let yesterday = out.find("──── Sat, Jun 27 2026 ────").expect("yesterday");
        let today = out.find("──── Today ").expect("today");
        let ask = out.find("Approve tool?").expect("ask");
        assert!(yesterday < today);
        assert!(today < ask);
    }
}
