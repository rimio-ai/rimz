//! `rimz transcript` — inspect agent and channel conversations from local logs.

use std::collections::{BTreeSet, HashMap};
use std::io::Write;

use anyhow::{Context, Result, bail};
use clap::Args;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde::Serialize;
use unicode_width::UnicodeWidthStr;

use super::{GlobalFlags, current_channel};
use crate::cli::render;
use rimz::agents::AskOption;
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
struct RenderEntry {
    kind: TranscriptKind,
    request_id: Option<RequestId>,
    agent: AgentKey,
    chat: rimz::agents::ChatEntry,
}

#[derive(Clone, Debug)]
struct Identity {
    base_handle: String,
    channel: Option<String>,
    name: Option<String>,
    profile: Option<String>,
    role: Option<String>,
    last_at: jiff::Timestamp,
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
    let live_root_keys = live_root_agent_keys(&workspace);
    let scope = resolve_scope(
        args.target.as_deref(),
        args.worktree.as_deref(),
        current.as_deref(),
        &identities,
        &live_root_keys,
    )?;
    let filtered: Vec<&TranscriptEntry> = entries
        .iter()
        .filter(|entry| entry_in_scope(entry, &scope))
        .collect();
    if filtered.is_empty() {
        bail!("no transcripts on disk for this scope yet");
    }

    let mut entries: Vec<_> = filtered
        .into_iter()
        .filter_map(|entry| {
            let render = render_entry_for_log_entry(entry, &identities, scope.include_channel);
            entry_matches_focus(entry, &render.chat, &scope, &identities).then_some(render)
        })
        .collect();
    entries.sort_by(|left, right| compare_optional_timestamps(left.chat.at, right.chat.at));

    if entries.is_empty() {
        bail!("no transcripts on disk for this scope yet");
    }
    keep_last(&mut entries, args.last);

    if args.json {
        print_json(&ChatView {
            channel: scope.channel,
            focus: scope.focus,
            entries: entries.iter().map(|entry| entry.chat.clone()).collect(),
        })?;
    } else {
        let tz = super::machine_config().time_zone();
        render_chat(scope.channel.as_deref(), &entries, &tz)?;
    }
    Ok(())
}

fn live_root_agent_keys(workspace: &rimz::ResolvedWorkspace) -> BTreeSet<AgentKey> {
    crate::cli::open_ledger(workspace)
        .ok()
        .and_then(|ledger| ledger.snapshot_cached().ok())
        .map(|snapshot| {
            snapshot
                .agents
                .into_iter()
                .filter(|agent| agent.parent_agent_id.is_none())
                .map(|agent| (agent.kind, agent.agent_id))
                .collect()
        })
        .unwrap_or_default()
}

fn entry_in_scope(entry: &TranscriptEntry, scope: &Scope) -> bool {
    scope
        .focus_keys
        .as_ref()
        .is_some_and(|focus| focus.contains(&entry_key(entry)))
        || channel_matches(entry.channel.as_deref(), scope.channel_filter.as_deref())
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

fn render_entry_for_log_entry(
    entry: &TranscriptEntry,
    identities: &HashMap<AgentKey, Identity>,
    include_channel: bool,
) -> RenderEntry {
    RenderEntry {
        kind: entry.entry,
        request_id: entry.request_id.clone(),
        agent: entry_key(entry),
        chat: chat_entry_for_log_entry(entry, identities, include_channel),
    }
}

fn chat_entry_for_log_entry(
    entry: &TranscriptEntry,
    identities: &HashMap<AgentKey, Identity>,
    include_channel: bool,
) -> rimz::agents::ChatEntry {
    let receiver = handle_for(entry, identities, include_channel);
    match entry.entry {
        TranscriptKind::Prompt => rimz::agents::ChatEntry {
            from: "user".to_owned(),
            to: Some(receiver),
            at: Some(entry.at),
            text: entry.text.clone(),
            error: false,
            questions: entry.questions.clone(),
            answers: entry.answers.clone(),
        },
        TranscriptKind::Message => rimz::agents::ChatEntry {
            from: entry.from.clone().unwrap_or_else(|| "user".to_owned()),
            to: Some(receiver),
            at: Some(entry.at),
            text: entry.text.clone(),
            error: false,
            questions: entry.questions.clone(),
            answers: entry.answers.clone(),
        },
        TranscriptKind::Assistant | TranscriptKind::Ask => rimz::agents::ChatEntry {
            from: receiver,
            to: None,
            at: Some(entry.at),
            text: entry.text.clone(),
            error: false,
            questions: entry.questions.clone(),
            answers: entry.answers.clone(),
        },
        TranscriptKind::Error => rimz::agents::ChatEntry {
            from: receiver,
            to: None,
            at: Some(entry.at),
            text: entry.text.clone(),
            error: true,
            questions: Vec::new(),
            answers: Vec::new(),
        },
        TranscriptKind::Answer => rimz::agents::ChatEntry {
            from: entry.from.clone().unwrap_or_else(|| "resolver".to_owned()),
            to: Some(receiver),
            at: Some(entry.at),
            text: entry.text.clone(),
            error: false,
            questions: entry.questions.clone(),
            answers: entry.answers.clone(),
        },
    }
}

fn entry_matches_focus(
    entry: &TranscriptEntry,
    chat: &rimz::agents::ChatEntry,
    scope: &Scope,
    identities: &HashMap<AgentKey, Identity>,
) -> bool {
    scope.focus_keys.as_ref().is_none_or(|focus| {
        focus.contains(&entry_key(entry))
            || sender_matches_focus(
                &chat.from,
                focus,
                identities,
                scope.channel_filter.as_deref(),
            )
    })
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
            base_handle: rimz::harness::target::identity_handle(
                &entry.kind,
                entry.name.as_deref(),
                entry.profile.as_deref(),
                entry.role.as_deref(),
            ),
            channel: entry.channel.clone(),
            name: entry.name.clone(),
            profile: entry.profile.clone(),
            role: entry.role.clone(),
            last_at: entry.at,
            rich: entry.role.is_some() || entry.name.is_some() || entry.profile.is_some(),
        };
        identities
            .entry(entry_key(entry))
            .and_modify(|existing: &mut Identity| {
                existing.last_at = existing.last_at.max(candidate.last_at);
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
    live_root_keys: &BTreeSet<AgentKey>,
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
            reconcile_transcript_channel(raw, Some(channel), worktree, None)?;
            Ok(single_channel_scope(channel.to_owned()))
        }
        Some(raw) if raw == "@all" || raw.starts_with("@all#") => {
            let (_, inline) = parse_transcript_target(raw)?;
            let channel = reconcile_transcript_channel(raw, inline.as_deref(), worktree, None)?;
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
            let (selector, inline) = parse_transcript_target(raw)?;
            let exact_session = exact_session_selector(&selector, identities);
            let requested_channel =
                reconcile_transcript_channel(raw, inline.as_deref(), worktree, current)?;
            let resolution_channel = (!exact_session)
                .then_some(requested_channel.as_deref())
                .flatten();
            let matches = matching_identities(&selector, resolution_channel, identities);
            let Some((key, identity)) = select_identity_match(&matches, live_root_keys) else {
                bail!("no agent matches target `{raw}` in the transcript log");
            };
            let channel = if exact_session {
                identity.channel.clone()
            } else {
                requested_channel.or_else(|| identity.channel.clone())
            };
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

fn parse_transcript_target(raw: &str) -> Result<(String, Option<String>)> {
    if matches!(raw.split_once('#'), Some((_, ""))) {
        bail!("channel suffix in target `{raw}` must name a channel");
    }
    match rimz::harness::target::parse_selector(raw) {
        Ok(parsed) => Ok(parsed),
        Err(rimz::TargetErr::NoMatch { .. } | rimz::TargetErr::InvalidPaneId(_)) => {
            Ok(split_transcript_target(raw))
        }
        Err(err) => Err(err.into()),
    }
}

fn split_transcript_target(raw: &str) -> (String, Option<String>) {
    raw.split_once('#').map_or_else(
        || (raw.to_owned(), None),
        |(selector, channel)| (selector.to_owned(), Some(channel.to_owned())),
    )
}

fn reconcile_transcript_channel(
    raw: &str,
    inline: Option<&str>,
    flag: Option<&str>,
    fallback: Option<&str>,
) -> Result<Option<String>> {
    match rimz::harness::target::reconcile_channel(raw, inline, flag, fallback) {
        Ok(channel) => Ok(channel),
        Err(rimz::TargetErr::ChannelMismatch {
            target,
            channel,
            flag,
        }) => bail!("target `{target}` names channel `#{channel}` but --worktree names `{flag}`"),
        Err(err) => Err(err.into()),
    }
}

fn matching_identities<'a>(
    selector: &str,
    channel: Option<&str>,
    identities: &'a HashMap<AgentKey, Identity>,
) -> Vec<(&'a AgentKey, &'a Identity)> {
    let selector = selector.strip_prefix('@').unwrap_or(selector);
    let mut exact: Vec<_> = identities
        .iter()
        .filter(|(key, _)| key.1.as_str() == selector)
        .collect();
    if !exact.is_empty() {
        exact.sort_by(|left, right| {
            candidate_label(left.0, left.1).cmp(&candidate_label(right.0, right.1))
        });
        return exact;
    }
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

fn exact_session_selector(selector: &str, identities: &HashMap<AgentKey, Identity>) -> bool {
    let selector = selector.strip_prefix('@').unwrap_or(selector);
    identities.keys().any(|key| key.1.as_str() == selector)
}

fn select_identity_match<'a>(
    matches: &[(&'a AgentKey, &'a Identity)],
    live_root_keys: &BTreeSet<AgentKey>,
) -> Option<(&'a AgentKey, &'a Identity)> {
    let pool: Vec<_> = if matches.iter().any(|(key, _)| live_root_keys.contains(*key)) {
        matches
            .iter()
            .copied()
            .filter(|(key, _)| live_root_keys.contains(*key))
            .collect()
    } else {
        matches.to_vec()
    };
    pool.into_iter().max_by(|left, right| {
        left.1
            .last_at
            .cmp(&right.1.last_at)
            .then_with(|| left.0.1.as_str().cmp(right.0.1.as_str()))
    })
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
    let base = rimz::harness::target::identity_handle(&entry.kind, None, None, None);
    render_handle(&base, entry.channel.as_deref(), include_channel)
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
const BODY_INDENT: &str = "  ";
const GROUP_WINDOW_SECS: i64 = 5 * 60;
const MAX_CARD_WIDTH: usize = 100;
const MIN_CARD_CONTENT_WIDTH: usize = 24;

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

fn render_chat(channel: Option<&str>, entries: &[RenderEntry], tz: &TimeZone) -> Result<()> {
    let mut out = render::out();
    let today = jiff::Timestamp::now().to_zoned(tz.clone()).date();
    render_chat_to(&mut out, channel, entries, tz, today)
}

fn render_chat_to(
    out: &mut impl Write,
    channel: Option<&str>,
    entries: &[RenderEntry],
    tz: &TimeZone,
    today: Date,
) -> Result<()> {
    write_header(out, channel)?;
    let grouped = channel.is_some();
    let folded = pair_answers(entries);
    let mut tones = AgentTones::default();
    let mut last_date = Some(today);
    let mut first_entry = true;
    let mut follows_day_delimiter = false;
    let mut last_group: Option<GroupState> = None;
    for (index, entry) in entries.iter().enumerate() {
        if folded.suppressed_answers.contains(&index) {
            continue;
        }
        let entry_date = entry.chat.at.map(|at| at.to_zoned(tz.clone()).date());
        if let Some(date) = entry_date
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
                .is_some_and(|group| group.matches(entry, grouped, entry_date));
        if !continuation && !first_entry && !follows_day_delimiter {
            writeln!(out)?;
        }
        if !continuation {
            write_entry_header(out, entry, grouped, &mut tones, tz)?;
        }
        if is_ask {
            let answer = folded
                .answer_by_ask
                .get(&index)
                .map(|answer| &entries[*answer]);
            write_ask_card(out, entry, answer)?;
            last_group = None;
        } else {
            if entry.chat.error {
                write_body_lines_with(out, &entry.chat.text, Some(render::palette::ALARM))?;
            } else {
                write_body_lines(out, &entry.chat.text)?;
            }
            last_group = Some(GroupState::new(entry, grouped, entry_date));
        }
        first_entry = false;
        follows_day_delimiter = false;
    }
    Ok(())
}

#[derive(Default)]
struct AnswerPairs {
    answer_by_ask: HashMap<usize, usize>,
    suppressed_answers: BTreeSet<usize>,
}

fn pair_answers(entries: &[RenderEntry]) -> AnswerPairs {
    let mut folded = AnswerPairs::default();
    let mut open_by_request: HashMap<RequestId, usize> = HashMap::new();
    let mut open_by_agent: HashMap<AgentKey, Vec<usize>> = HashMap::new();
    for (index, entry) in entries.iter().enumerate() {
        match entry.kind {
            TranscriptKind::Ask => {
                if let Some(request_id) = entry.request_id.as_ref() {
                    open_by_request.insert(request_id.clone(), index);
                }
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
                let ask = if let Some(request_id) = entry.request_id.as_ref() {
                    open_by_request
                        .remove(request_id)
                        .filter(|ask| !folded.answer_by_ask.contains_key(ask))
                } else {
                    by_agent()
                };
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
struct GroupState {
    from: String,
    to: Option<String>,
    at: Option<jiff::Timestamp>,
    date: Option<Date>,
}

impl GroupState {
    fn new(entry: &RenderEntry, grouped: bool, date: Option<Date>) -> Self {
        let (from, to) = group_key(entry, grouped);
        Self {
            from,
            to,
            at: entry.chat.at,
            date,
        }
    }

    fn matches(&self, entry: &RenderEntry, grouped: bool, date: Option<Date>) -> bool {
        let (from, to) = group_key(entry, grouped);
        if self.from != from || self.to != to || self.date != date {
            return false;
        }
        let (Some(previous), Some(current)) = (self.at, entry.chat.at) else {
            return false;
        };
        let gap = current.duration_since(previous);
        !gap.is_negative() && gap.as_secs() <= GROUP_WINDOW_SECS
    }
}

fn group_key(entry: &RenderEntry, grouped: bool) -> (String, Option<String>) {
    (
        display_handle(&entry.chat.from, grouped).to_owned(),
        entry
            .chat
            .to
            .as_deref()
            .map(|to| display_handle(to, grouped).to_owned()),
    )
}

fn write_entry_header(
    out: &mut impl Write,
    entry: &RenderEntry,
    grouped: bool,
    tones: &mut AgentTones,
    tz: &TimeZone,
) -> Result<()> {
    let mut header = paint_handle(&entry.chat.from, grouped, tones);
    if let Some(to) = entry.chat.to.as_deref() {
        header.push_str(&render::paint(render::palette::FAINT, " → "));
        header.push_str(&paint_handle(to, grouped, tones));
    }
    if let Some(at) = entry.chat.at {
        header.push_str("  ");
        header.push_str(&render::paint(
            render::palette::FAINT,
            &at.to_zoned(tz.clone()).strftime("%H:%M").to_string(),
        ));
    }
    writeln!(out, "{header}")?;
    Ok(())
}

fn paint_handle(handle: &str, grouped: bool, tones: &mut AgentTones) -> String {
    if handle == "user" {
        render::paint(render::palette::COOL, "user")
    } else if handle == "you" {
        render::paint(render::palette::COOL.bold(), "you")
    } else {
        let display = display_handle(handle, grouped);
        render::paint(tones.tone(base_handle(handle)).bold(), display)
    }
}

fn write_body_lines(out: &mut impl Write, text: &str) -> Result<()> {
    write_body_lines_with(out, text, None)
}

fn write_body_lines_with(
    out: &mut impl Write,
    text: &str,
    style: Option<anstyle::Style>,
) -> Result<()> {
    for line in text.lines() {
        if line.is_empty() {
            writeln!(out)?;
        } else {
            writeln!(out, "{BODY_INDENT}{}", paint_mentions_with(line, style))?;
        }
    }
    Ok(())
}

fn paint_mentions_with(line: &str, base_style: Option<anstyle::Style>) -> String {
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
                let style = if ch == '@' {
                    render::palette::COOL.bold()
                } else {
                    render::palette::ACCENT.bold()
                };
                push_painted(&mut rendered, base_style, &line[..index]);
                rendered.push_str(&render::paint(style, &line[index..paint_end]));
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

fn push_painted(rendered: &mut String, style: Option<anstyle::Style>, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(style) = style {
        rendered.push_str(&render::paint(style, text));
    } else {
        rendered.push_str(text);
    }
}

fn mention_boundary(line: &str, index: usize) -> bool {
    if index == 0 {
        return true;
    }
    line[..index]
        .chars()
        .next_back()
        .is_some_and(|ch| ch.is_whitespace() || ch == '(')
}

fn is_mention_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '/' | '-')
}

#[derive(Clone)]
struct StyledFragment {
    text: String,
    style: Option<anstyle::Style>,
    mentions: bool,
}

impl StyledFragment {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: None,
            mentions: false,
        }
    }

    fn styled(text: impl Into<String>, style: anstyle::Style) -> Self {
        Self {
            text: text.into(),
            style: Some(style),
            mentions: false,
        }
    }

    fn prose(text: impl Into<String>, style: Option<anstyle::Style>) -> Self {
        Self {
            text: text.into(),
            style,
            mentions: true,
        }
    }
}

#[derive(Clone)]
struct WrapToken {
    text: String,
    style: Option<anstyle::Style>,
    mentions: bool,
}

fn card_content_width() -> usize {
    let terminal = terminal_size::terminal_size()
        .map(|(terminal_size::Width(width), _)| usize::from(width))
        .unwrap_or(MAX_CARD_WIDTH)
        .min(MAX_CARD_WIDTH);
    let prefix_width = UnicodeWidthStr::width(format!("{BODY_INDENT}▌ ").as_str());
    terminal
        .saturating_sub(prefix_width)
        .max(MIN_CARD_CONTENT_WIDTH)
}

fn write_wrapped_spine_fragments(
    out: &mut impl Write,
    answered: bool,
    fragments: Vec<StyledFragment>,
    hang_indent: &str,
) -> Result<()> {
    write_wrapped_spine_fragments_with_first_indent(out, answered, fragments, "", hang_indent)
}

fn write_wrapped_spine_fragments_with_first_indent(
    out: &mut impl Write,
    answered: bool,
    fragments: Vec<StyledFragment>,
    first_indent: &str,
    hang_indent: &str,
) -> Result<()> {
    for line in wrap_fragments(fragments, card_content_width(), first_indent, hang_indent) {
        write_spine_fragments(out, answered, &line)?;
    }
    Ok(())
}

fn wrap_fragments(
    fragments: Vec<StyledFragment>,
    width: usize,
    first_indent: &str,
    hang_indent: &str,
) -> Vec<Vec<StyledFragment>> {
    let tokens = fragment_tokens(fragments);
    if tokens.is_empty() {
        return vec![if first_indent.is_empty() {
            Vec::new()
        } else {
            vec![StyledFragment::plain(first_indent)]
        }];
    }

    let mut lines = Vec::new();
    let first_width = UnicodeWidthStr::width(first_indent);
    let mut current = if first_indent.is_empty() {
        Vec::new()
    } else {
        vec![StyledFragment::plain(first_indent)]
    };
    let mut current_width = first_width;
    let mut has_word = false;
    let hang_width = UnicodeWidthStr::width(hang_indent);

    for token in tokens {
        let token_width = UnicodeWidthStr::width(token.text.as_str());
        let separator_width = usize::from(has_word);
        if has_word && current_width + separator_width + token_width > width {
            lines.push(current);
            current = Vec::new();
            current_width = 0;
            has_word = false;
            if !hang_indent.is_empty() {
                current.push(StyledFragment::plain(hang_indent));
                current_width = hang_width;
            }
        }
        if has_word {
            current.push(StyledFragment::plain(" "));
            current_width += 1;
        }
        current.push(StyledFragment {
            text: token.text,
            style: token.style,
            mentions: token.mentions,
        });
        current_width += token_width;
        has_word = true;
    }
    lines.push(current);
    lines
}

fn fragment_tokens(fragments: Vec<StyledFragment>) -> Vec<WrapToken> {
    fragments
        .into_iter()
        .flat_map(|fragment| {
            fragment
                .text
                .split_whitespace()
                .map(|word| WrapToken {
                    text: word.to_owned(),
                    style: fragment.style,
                    mentions: fragment.mentions,
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

fn write_spine_fragments(
    out: &mut impl Write,
    answered: bool,
    fragments: &[StyledFragment],
) -> Result<()> {
    let style = if answered {
        render::palette::FAINT
    } else {
        render::palette::WARN
    };
    write!(out, "{BODY_INDENT}{}", render::paint(style, "▌ "))?;
    for fragment in fragments {
        if fragment.mentions {
            write!(
                out,
                "{}",
                paint_mentions_with(&fragment.text, fragment.style)
            )?;
        } else if let Some(style) = fragment.style {
            write!(out, "{}", render::paint(style, &fragment.text))?;
        } else {
            write!(out, "{}", fragment.text)?;
        }
    }
    writeln!(out)?;
    Ok(())
}

struct ParsedLegacyAsk {
    lead_in: String,
    questions: Vec<rimz::agents::AskQuestion>,
}

fn parse_legacy_flattened_ask(text: &str) -> Option<ParsedLegacyAsk> {
    let lines = text.lines().collect::<Vec<_>>();
    let mut end = lines.len();
    while end > 0 && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && !lines[start - 1].trim().is_empty() {
        start -= 1;
    }
    if start == end {
        return None;
    }
    let questions = lines[start..end]
        .iter()
        .map(|line| parse_legacy_question_line(line.trim()))
        .collect::<Option<Vec<_>>>()?;
    let mut lead = lines[..start].to_vec();
    while lead.last().is_some_and(|line| line.trim().is_empty()) {
        lead.pop();
    }
    Some(ParsedLegacyAsk {
        lead_in: lead.join("\n"),
        questions,
    })
}

fn parse_legacy_question_line(line: &str) -> Option<rimz::agents::AskQuestion> {
    let inner = line.strip_suffix(']')?;
    let (question, options) = inner.rsplit_once(" [")?;
    let question = non_empty(question)?.to_owned();
    let options = options
        .split(", ")
        .map(str::trim)
        .filter(|option| !option.is_empty())
        .map(|option| AskOption::from(option.to_owned()))
        .collect::<Vec<_>>();
    (!options.is_empty()).then_some(rimz::agents::AskQuestion { question, options })
}

fn folded_answers_for_legacy<'a>(
    answer: Option<&'a RenderEntry>,
    questions: &[rimz::agents::AskQuestion],
) -> Option<(Vec<rimz::agents::AskAnswer>, Option<&'a str>)> {
    let Some(answer) = answer else {
        return Some((Vec::new(), None));
    };
    if !answer.chat.answers.is_empty() {
        return Some((answer.chat.answers.clone(), Some(answer.chat.from.as_str())));
    }
    let answers = parse_legacy_answer_text(&answer.chat.text, questions)?;
    Some((answers, Some(answer.chat.from.as_str())))
}

fn parse_legacy_answer_text(
    text: &str,
    questions: &[rimz::agents::AskQuestion],
) -> Option<Vec<rimz::agents::AskAnswer>> {
    let lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if lines.len() > questions.len() {
        return None;
    }
    let mut answers = Vec::new();
    for (line, question) in lines.into_iter().zip(questions) {
        let (line, note) = strip_legacy_note(line);
        let chosen = if question
            .options
            .iter()
            .any(|option| option.label.as_str() == line)
        {
            vec![line]
        } else {
            let parts = line
                .split(", ")
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>();
            if parts.len() > 1
                && parts.iter().all(|part| {
                    question
                        .options
                        .iter()
                        .any(|option| option.label.as_str() == *part)
                })
            {
                parts.into_iter().map(ToOwned::to_owned).collect()
            } else {
                vec![line]
            }
        };
        answers.push(rimz::agents::AskAnswer {
            question: Some(question.question.clone()),
            chosen,
            note,
        });
    }
    Some(answers)
}

fn strip_legacy_note(line: &str) -> (String, Option<String>) {
    if let Some(inner) = line.strip_suffix(')')
        && let Some((answer, note)) = inner.rsplit_once(" (note: ")
        && let Some(answer) = non_empty(answer)
        && let Some(note) = non_empty(note)
    {
        return (answer.to_owned(), Some(note.to_owned()));
    }
    (line.to_owned(), None)
}

fn write_ask_card(
    out: &mut impl Write,
    ask: &RenderEntry,
    answer: Option<&RenderEntry>,
) -> Result<()> {
    if ask.chat.questions.is_empty() {
        if let Some(parsed) = parse_legacy_flattened_ask(&ask.chat.text)
            && let Some((answers, source)) = folded_answers_for_legacy(answer, &parsed.questions)
        {
            if !parsed.lead_in.is_empty() {
                write_body_lines(out, &parsed.lead_in)?;
            }
            write_structured_ask_card_with_answers(out, &parsed.questions, &answers, source)
        } else {
            write_legacy_text_card(out, ask, answer)
        }
    } else {
        if !ask.chat.text.is_empty() {
            write_body_lines(out, &ask.chat.text)?;
        }
        write_structured_ask_card(out, &ask.chat.questions, answer)
    }
}

fn write_structured_ask_card(
    out: &mut impl Write,
    questions: &[rimz::agents::AskQuestion],
    answer: Option<&RenderEntry>,
) -> Result<()> {
    let (answers, source) = folded_answers(answer);
    write_structured_ask_card_with_answers(out, questions, &answers, source)
}

fn write_structured_ask_card_with_answers(
    out: &mut impl Write,
    questions: &[rimz::agents::AskQuestion],
    answers: &[rimz::agents::AskAnswer],
    source: Option<&str>,
) -> Result<()> {
    let matched = match_question_answers(questions, answers);
    for (index, question) in questions.iter().enumerate() {
        if index > 0 {
            write_spine_blank(out, matched[index - 1].is_some())?;
        }
        write_question_block(out, question, matched[index].as_ref(), source)?;
    }
    Ok(())
}

fn folded_answers(answer: Option<&RenderEntry>) -> (Vec<rimz::agents::AskAnswer>, Option<&str>) {
    let Some(answer) = answer else {
        return (Vec::new(), None);
    };
    let answers = if !answer.chat.answers.is_empty() {
        answer.chat.answers.clone()
    } else {
        let text = answer.chat.text.trim();
        if text.is_empty() {
            Vec::new()
        } else {
            vec![rimz::agents::AskAnswer {
                question: None,
                chosen: vec![text.to_owned()],
                note: None,
            }]
        }
    };
    (answers, Some(answer.chat.from.as_str()))
}

fn match_question_answers(
    questions: &[rimz::agents::AskQuestion],
    answers: &[rimz::agents::AskAnswer],
) -> Vec<Option<rimz::agents::AskAnswer>> {
    let mut matched = vec![None; questions.len()];
    let mut used = vec![false; answers.len()];
    for (answer_index, answer) in answers.iter().enumerate() {
        let Some(question) = answer.question.as_deref() else {
            continue;
        };
        if let Some(question_index) = questions.iter().enumerate().position(|(index, candidate)| {
            candidate.question == question && matched[index].is_none()
        }) {
            matched[question_index] = Some(answer.clone());
            used[answer_index] = true;
        }
    }
    for (answer_index, answer) in answers.iter().enumerate() {
        if used[answer_index] {
            continue;
        }
        if let Some(slot) = matched.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(answer.clone());
        }
    }
    matched
}

fn write_question_block(
    out: &mut impl Write,
    question: &rimz::agents::AskQuestion,
    answer: Option<&rimz::agents::AskAnswer>,
    source: Option<&str>,
) -> Result<()> {
    let answered = answer.is_some();
    write_question_text(out, answered, &question.question)?;
    match answer {
        Some(answer) if question.options.is_empty() => write_free_answer(out, answer, source),
        Some(answer) => write_option_answers(out, &question.options, answer, source),
        None => {
            for option in &question.options {
                write_wrapped_spine_fragments(
                    out,
                    false,
                    vec![StyledFragment::plain(format!("○ {}", option.label))],
                    "  ",
                )?;
                write_option_description(out, false, option)?;
            }
            write_unanswered(out)
        }
    }
}

fn write_question_text(out: &mut impl Write, answered: bool, question: &str) -> Result<()> {
    for (index, line) in question.lines().enumerate() {
        let style = if index == 0 {
            Some(anstyle::Style::new().bold())
        } else {
            None
        };
        write_wrapped_spine_fragments(out, answered, vec![StyledFragment::prose(line, style)], "")?;
    }
    Ok(())
}

fn write_free_answer(
    out: &mut impl Write,
    answer: &rimz::agents::AskAnswer,
    source: Option<&str>,
) -> Result<()> {
    let mut suffix_written = false;
    for choice in answer.chosen.iter().filter_map(|choice| non_empty(choice)) {
        let mut fragments = vec![
            StyledFragment::styled("●", render::palette::GOOD.bold()),
            StyledFragment::prose(choice, Some(render::palette::GOOD.bold())),
        ];
        if let Some(suffix) =
            answer_suffix_text(source, answer.note.as_deref(), &mut suffix_written)
        {
            fragments.push(StyledFragment::styled(suffix, render::palette::MUTED));
        }
        write_wrapped_spine_fragments(out, true, fragments, "  ")?;
    }
    Ok(())
}

fn write_option_answers(
    out: &mut impl Write,
    options: &[AskOption],
    answer: &rimz::agents::AskAnswer,
    source: Option<&str>,
) -> Result<()> {
    let chosen = answer
        .chosen
        .iter()
        .filter_map(|choice| non_empty(choice))
        .collect::<Vec<_>>();
    let mut suffix_written = false;
    for option in options {
        if chosen.contains(&option.label.as_str()) {
            let mut fragments = vec![
                StyledFragment::styled("●", render::palette::GOOD.bold()),
                StyledFragment::styled(option.label.clone(), render::palette::GOOD.bold()),
            ];
            if let Some(suffix) =
                answer_suffix_text(source, answer.note.as_deref(), &mut suffix_written)
            {
                fragments.push(StyledFragment::styled(suffix, render::palette::MUTED));
            }
            write_wrapped_spine_fragments(out, true, fragments, "  ")?;
            write_option_description(out, true, option)?;
        } else {
            write_wrapped_spine_fragments(
                out,
                true,
                vec![StyledFragment::styled(
                    format!("○ {}", option.label),
                    render::palette::MUTED,
                )],
                "  ",
            )?;
            write_option_description(out, true, option)?;
        }
    }
    let other = chosen
        .into_iter()
        .filter(|choice| {
            !options
                .iter()
                .any(|option| option.label.as_str() == *choice)
        })
        .collect::<Vec<_>>();
    if !other.is_empty() {
        let mut fragments = vec![
            StyledFragment::styled("●", render::palette::GOOD.bold()),
            StyledFragment::styled("other:", render::palette::MUTED),
            StyledFragment::prose(other.join(", "), Some(render::palette::GOOD.bold())),
        ];
        if let Some(suffix) =
            answer_suffix_text(source, answer.note.as_deref(), &mut suffix_written)
        {
            fragments.push(StyledFragment::styled(suffix, render::palette::MUTED));
        }
        write_wrapped_spine_fragments(out, true, fragments, "  ")?;
    }
    Ok(())
}

fn write_option_description(
    out: &mut impl Write,
    answered: bool,
    option: &AskOption,
) -> Result<()> {
    let Some(description) = option.description.as_deref().and_then(non_empty) else {
        return Ok(());
    };
    for line in description.lines().filter_map(non_empty) {
        write_wrapped_spine_fragments_with_first_indent(
            out,
            answered,
            vec![StyledFragment::styled(line, render::palette::FAINT)],
            "    ",
            "    ",
        )?;
    }
    Ok(())
}

fn write_legacy_text_card(
    out: &mut impl Write,
    ask: &RenderEntry,
    answer: Option<&RenderEntry>,
) -> Result<()> {
    let answered = answer.is_some();
    for line in ask.chat.text.lines() {
        write_wrapped_spine_fragments(out, answered, vec![StyledFragment::prose(line, None)], "")?;
    }
    let Some(answer) = answer else {
        return write_unanswered(out);
    };
    let text = if answer.chat.answers.is_empty() {
        answer.chat.text.trim().to_owned()
    } else {
        rimz::agents::answers_text(&answer.chat.answers)
    };
    if text.is_empty() {
        return Ok(());
    }
    let mut suffix_written = false;
    for line in text.lines() {
        let mut fragments = vec![
            StyledFragment::styled("●", render::palette::GOOD.bold()),
            StyledFragment::prose(line, Some(render::palette::GOOD.bold())),
        ];
        if let Some(suffix) = answer_suffix_text(Some(&answer.chat.from), None, &mut suffix_written)
        {
            fragments.push(StyledFragment::styled(suffix, render::palette::MUTED));
        }
        write_wrapped_spine_fragments(out, true, fragments, "  ")?;
    }
    Ok(())
}

fn write_unanswered(out: &mut impl Write) -> Result<()> {
    write_wrapped_spine_fragments(
        out,
        false,
        vec![StyledFragment::styled(
            "◌ unanswered",
            render::palette::WARN,
        )],
        "",
    )
}

fn answer_suffix_text(
    source: Option<&str>,
    note: Option<&str>,
    written: &mut bool,
) -> Option<String> {
    if *written {
        return None;
    }
    *written = true;
    let source = source.and_then(non_empty)?;
    let mut suffix = format!(" — {source}");
    if let Some(note) = note.and_then(non_empty) {
        suffix.push_str(" · “");
        suffix.push_str(note);
        suffix.push('”');
    }
    Some(suffix)
}

fn non_empty(text: &str) -> Option<&str> {
    let text = text.trim();
    (!text.is_empty()).then_some(text)
}

fn write_spine_blank(out: &mut impl Write, answered: bool) -> Result<()> {
    let style = if answered {
        render::palette::FAINT
    } else {
        render::palette::WARN
    };
    writeln!(out, "{BODY_INDENT}{}", render::paint(style, "▌"))?;
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

fn print_json(value: &impl Serialize) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}

#[cfg(test)]
mod retry_on_error;

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(raw: &str) -> jiff::Timestamp {
        raw.parse().expect("timestamp")
    }

    fn agent_key() -> AgentKey {
        (
            AgentKind::new_unchecked("claude"),
            AgentSessionId::from("sess-1"),
        )
    }

    fn render_entry(
        kind: TranscriptKind,
        from: &str,
        to: Option<&str>,
        at: &str,
        text: &str,
    ) -> RenderEntry {
        RenderEntry {
            kind,
            request_id: None,
            agent: agent_key(),
            chat: rimz::agents::ChatEntry {
                from: from.to_owned(),
                to: to.map(ToOwned::to_owned),
                at: Some(ts(at)),
                text: text.to_owned(),
                error: kind == TranscriptKind::Error,
                questions: Vec::new(),
                answers: Vec::new(),
            },
        }
    }

    fn entry(at: &str, text: &str) -> RenderEntry {
        render_entry(TranscriptKind::Prompt, "user", Some("@claude"), at, text)
    }

    fn assistant_entry(at: &str, text: &str) -> RenderEntry {
        render_entry(TranscriptKind::Assistant, "@claude", None, at, text)
    }

    fn ask_entry(at: &str, text: &str) -> RenderEntry {
        render_entry(TranscriptKind::Ask, "@claude", None, at, text)
    }

    fn answer_entry(at: &str, text: &str) -> RenderEntry {
        render_entry(TranscriptKind::Answer, "you", Some("@claude"), at, text)
    }

    fn ask_option(label: &str) -> AskOption {
        AskOption::from(label.to_owned())
    }

    fn described_ask_option(label: &str, description: &str) -> AskOption {
        AskOption {
            label: label.to_owned(),
            description: Some(description.to_owned()),
        }
    }

    fn render(entries: &[RenderEntry], today: Date) -> String {
        let tz = TimeZone::get("America/New_York").expect("timezone");
        let mut out = anstream::StripStream::new(Vec::new());
        render_chat_to(&mut out, None, entries, &tz, today).expect("render");
        String::from_utf8(out.into_inner()).expect("utf8")
    }

    fn render_raw(entries: &[RenderEntry], today: Date) -> String {
        let tz = TimeZone::get("America/New_York").expect("timezone");
        let mut out = Vec::new();
        render_chat_to(&mut out, None, entries, &tz, today).expect("render");
        String::from_utf8(out).expect("utf8")
    }

    fn log_entry(
        kind: &str,
        session_id: &str,
        entry: TranscriptKind,
        from: Option<&str>,
        text: &str,
    ) -> TranscriptEntry {
        TranscriptEntry {
            at: ts("2026-06-01T00:00:00Z"),
            kind: AgentKind::new_unchecked(kind),
            agent_id: AgentSessionId::from(session_id),
            channel: Some("chat".to_owned()),
            name: None,
            profile: None,
            role: None,
            entry,
            request_id: None,
            from: from.map(ToOwned::to_owned),
            text: text.to_owned(),
            questions: Vec::new(),
            answers: Vec::new(),
        }
    }

    #[test]
    fn message_entry_projects_structured_sender_and_receiver() {
        let entry = log_entry(
            "claude",
            "receiver",
            TranscriptKind::Message,
            Some("@planner"),
            "ship it",
        );
        let identities = build_identities(std::slice::from_ref(&entry));

        let chat = chat_entry_for_log_entry(&entry, &identities, false);

        assert_eq!(chat.from, "@planner");
        assert_eq!(chat.to.as_deref(), Some("@claude"));
        assert_eq!(chat.text, "ship it");
    }

    #[test]
    fn focus_keeps_messages_sent_by_the_focal_agent() {
        let focal = log_entry("claude", "sender", TranscriptKind::Prompt, None, "start");
        let sent = log_entry(
            "codex",
            "receiver",
            TranscriptKind::Message,
            Some("@claude"),
            "ack",
        );
        let local = log_entry("codex", "receiver", TranscriptKind::Prompt, None, "local");
        let identities = build_identities(&[focal.clone(), sent.clone()]);
        let scope = Scope {
            channel: Some("chat".to_owned()),
            channel_filter: Some("chat".to_owned()),
            focus: Some("@claude".to_owned()),
            focus_keys: Some(BTreeSet::from([entry_key(&focal)])),
            include_channel: false,
        };

        let sent_chat = chat_entry_for_log_entry(&sent, &identities, false);
        let local_chat = chat_entry_for_log_entry(&local, &identities, false);

        assert!(entry_matches_focus(&sent, &sent_chat, &scope, &identities));
        assert!(!entry_matches_focus(
            &local,
            &local_chat,
            &scope,
            &identities
        ));
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
        assert!(out.contains("23:30"));
        assert!(out.contains("10:00"));
        assert!(out.contains("12:00"));
        assert!(!out.contains("03:30:00"));
    }

    #[test]
    fn today_only_chat_omits_day_delimiter() {
        let out = render(
            &[entry("2026-06-28T04:30:00Z", "same day")],
            jiff::civil::date(2026, 6, 28),
        );

        assert!(!out.contains("────"));
        assert!(out.contains("00:30"));
    }

    #[test]
    fn chat_renders_speaker_headers_and_body_indent() {
        let out = render(
            &[
                entry("2026-06-28T04:00:00Z", "hello\n\nagain"),
                assistant_entry("2026-06-28T04:00:01Z", "answer"),
            ],
            jiff::civil::date(2026, 6, 28),
        );

        assert!(
            out.contains("user → @claude  00:00\n  hello\n\n  again\n\n@claude  00:00\n  answer"),
            "{out}"
        );
        assert!(!out.contains("user:"), "{out}");
    }

    #[test]
    fn chat_groups_same_sender_receiver_inside_window() {
        let out = render(
            &[
                entry("2026-06-28T04:00:00Z", "first"),
                entry("2026-06-28T04:04:59Z", "second"),
                entry("2026-06-28T04:10:01Z", "third"),
            ],
            jiff::civil::date(2026, 6, 28),
        );

        assert_eq!(out.matches("user → @claude").count(), 2, "{out}");
        assert!(out.contains("first\n  second"), "{out}");
    }

    #[test]
    fn chat_breaks_group_on_receiver_change_and_ask_cards() {
        let mut routed = entry("2026-06-28T04:02:00Z", "to codex");
        routed.chat.to = Some("@codex".to_owned());
        let out = render(
            &[
                entry("2026-06-28T04:00:00Z", "first"),
                routed,
                ask_entry("2026-06-28T04:03:00Z", "Approve tool?"),
                entry("2026-06-28T04:04:00Z", "after ask"),
            ],
            jiff::civil::date(2026, 6, 28),
        );

        assert_eq!(out.matches("user →").count(), 3, "{out}");
        assert!(out.contains("  ▌ Approve tool?\n  ▌ ◌ unanswered"), "{out}");
    }

    #[test]
    fn mention_painting_highlights_agents_and_channels() {
        let raw = paint_mentions_with("ping @codex in (#feat-auth). keep email@host plain", None);

        assert!(raw.contains("\u{1b}["), "{raw:?}");
        assert!(raw.contains("@codex"), "{raw}");
        assert!(raw.contains("#feat-auth"), "{raw}");
        assert!(raw.contains(&render::paint(render::palette::ACCENT.bold(), "#feat-auth")));
        assert!(raw.contains("email@host"), "{raw}");
    }

    #[test]
    fn structured_ask_card_folds_selected_answer_with_note() {
        let request_id = RequestId::new();
        let mut ask = ask_entry("2026-06-28T18:00:00Z", "I checked both paths.");
        ask.request_id = Some(request_id.clone());
        ask.chat.questions = vec![rimz::agents::AskQuestion {
            question: "Choose deployment path?".to_owned(),
            options: vec![
                described_ask_option("safe", "Use staged rollout with rollback ready."),
                described_ask_option("fast", "Ship immediately and monitor closely."),
            ],
        }];
        let mut answer = answer_entry("2026-06-28T18:01:00Z", "safe");
        answer.request_id = Some(request_id);
        answer.chat.answers = vec![rimz::agents::AskAnswer {
            question: Some("Choose deployment path?".to_owned()),
            chosen: vec!["safe".to_owned()],
            note: Some("use prod window".to_owned()),
        }];

        let out = render(&[ask, answer], jiff::civil::date(2026, 6, 28));

        assert!(
            out.contains("@claude  14:00\n  I checked both paths."),
            "{out}"
        );
        assert!(out.contains("  ▌ Choose deployment path?"), "{out}");
        assert!(
            out.contains("  ▌ ● safe — you · “use prod window”"),
            "{out}"
        );
        assert!(
            out.contains("  ▌     Use staged rollout with rollback ready."),
            "{out}"
        );
        assert!(out.contains("  ▌ ○ fast"), "{out}");
        assert!(
            out.contains("  ▌     Ship immediately and monitor closely."),
            "{out}"
        );
        assert!(!out.contains("you → @claude"), "{out}");

        let mut raw_ask = ask_entry("2026-06-28T18:00:00Z", "");
        raw_ask.chat.questions = vec![rimz::agents::AskQuestion {
            question: "Choose deployment path?".to_owned(),
            options: vec![
                described_ask_option("safe", "Tell @ops before rollout."),
                ask_option("fast"),
            ],
        }];
        let mut raw_answer = answer_entry("2026-06-28T18:01:00Z", "safe");
        raw_answer.chat.answers = vec![rimz::agents::AskAnswer {
            question: Some("Choose deployment path?".to_owned()),
            chosen: vec!["safe".to_owned()],
            note: None,
        }];
        let raw = render_raw(&[raw_ask, raw_answer], jiff::civil::date(2026, 6, 28));
        assert!(raw.contains(&render::paint(render::palette::GOOD.bold(), "safe")));
        assert!(raw.contains(&render::paint(render::palette::FAINT, "@ops")));
        assert!(!raw.contains(&render::paint(render::palette::COOL.bold(), "@ops")));
    }

    #[test]
    fn structured_ask_card_renders_other_and_multi_question_answers() {
        let mut ask = ask_entry("2026-06-28T18:00:00Z", "");
        ask.chat.questions = vec![
            rimz::agents::AskQuestion {
                question: "Merge strategy?".to_owned(),
                options: vec![ask_option("squash"), ask_option("rebase")],
            },
            rimz::agents::AskQuestion {
                question: "Notify team?".to_owned(),
                options: vec![ask_option("yes"), ask_option("no")],
            },
        ];
        let mut answer = answer_entry("2026-06-28T18:01:00Z", "live repro first\nyes");
        answer.chat.answers = vec![
            rimz::agents::AskAnswer {
                question: Some("Notify team?".to_owned()),
                chosen: vec!["yes".to_owned()],
                note: None,
            },
            rimz::agents::AskAnswer {
                question: Some("Merge strategy?".to_owned()),
                chosen: vec!["live repro first".to_owned()],
                note: None,
            },
        ];

        let out = render(&[ask, answer], jiff::civil::date(2026, 6, 28));

        assert!(out.contains("  ▌ ● other: live repro first — you"), "{out}");
        assert!(out.contains("  ▌\n  ▌ Notify team?"), "{out}");
        assert!(out.contains("  ▌ ● yes — you"), "{out}");
    }

    #[test]
    fn legacy_ask_card_folds_text_answer_or_stays_unanswered() {
        let mut ask = ask_entry("2026-06-28T18:00:00Z", "Choose path? [safe, fast]");
        ask.request_id = Some(RequestId::new());
        let mut answer = answer_entry("2026-06-28T18:01:00Z", "safe");
        answer.request_id = ask.request_id.clone();

        let answered = render(&[ask.clone(), answer], jiff::civil::date(2026, 6, 28));
        let unanswered = render(&[ask], jiff::civil::date(2026, 6, 28));

        assert!(
            answered.contains("  ▌ Choose path?\n  ▌ ● safe — you"),
            "{answered}"
        );
        assert!(answered.contains("  ▌ ○ fast"), "{answered}");
        assert!(
            unanswered.contains("  ▌ Choose path?\n  ▌ ○ safe\n  ▌ ○ fast\n  ▌ ◌ unanswered"),
            "{unanswered}"
        );
    }

    #[test]
    fn legacy_ask_card_parses_lead_in_notes_and_multiselect_answers() {
        let ask = ask_entry(
            "2026-06-28T18:00:00Z",
            "Here is my read.\n\nChoose scopes? [a, b, c]\nNotify #cli-docs? [yes, no]",
        );
        let mut answer = answer_entry("2026-06-28T18:01:00Z", "a, b (note: least risky)\nyes");
        answer.request_id = ask.request_id.clone();

        let out = render(&[ask, answer], jiff::civil::date(2026, 6, 28));

        assert!(
            out.contains("  Here is my read.\n  ▌ Choose scopes?"),
            "{out}"
        );
        assert!(out.contains("  ▌ ● a — you · “least risky”"), "{out}");
        assert!(out.contains("  ▌ ● b"), "{out}");
        assert!(out.contains("  ▌ ○ c"), "{out}");
        assert!(out.contains("  ▌ Notify #cli-docs?"), "{out}");
        assert!(out.contains("  ▌ ● yes — you"), "{out}");
    }

    #[test]
    fn legacy_exit_plan_text_falls_back_to_text_card() {
        let out = render(
            &[ask_entry(
                "2026-06-28T18:00:00Z",
                "Requesting plan approval:\n\n1. Edit parser\n2. Run tests",
            )],
            jiff::civil::date(2026, 6, 28),
        );

        assert!(out.contains("  ▌ Requesting plan approval:"), "{out}");
        assert!(out.contains("  ▌ 1. Edit parser"), "{out}");
        assert!(out.contains("  ▌ ◌ unanswered"), "{out}");
    }

    #[test]
    fn question_lines_paint_mentions() {
        let mut ask = ask_entry("2026-06-28T18:00:00Z", "");
        ask.chat.questions = vec![rimz::agents::AskQuestion {
            question: "Ask @codex about #cli-docs?".to_owned(),
            options: vec![ask_option("yes"), ask_option("no")],
        }];

        let raw = render_raw(&[ask], jiff::civil::date(2026, 6, 28));

        assert!(raw.contains(&render::paint(render::palette::COOL.bold(), "@codex")));
        assert!(raw.contains(&render::paint(render::palette::ACCENT.bold(), "#cli-docs")));
    }

    #[test]
    fn card_lines_wrap_with_spine_and_option_hanging_indent() {
        let mut ask = ask_entry("2026-06-28T18:00:00Z", "");
        ask.chat.questions = vec![rimz::agents::AskQuestion {
            question: "Which deployment plan should the release captain choose when the fallback window is narrow and every reviewer needs one clear sentence of context?".to_owned(),
            options: vec![
                described_ask_option(
                    "safe path with a carefully staged rollout and a rollback checkpoint before traffic moves while the on-call lead watches dashboards and keeps incident notes open",
                    "Choose this path when stakeholders need an especially detailed explanation that keeps wrapping under the option description indentation.",
                ),
                ask_option("fast path"),
            ],
        }];

        let out = render(&[ask], jiff::civil::date(2026, 6, 28));

        assert!(
            out.contains("\n  ▌ every reviewer needs one clear sentence"),
            "{out}"
        );
        assert!(
            out.contains("\n  ▌   the on-call lead watches dashboards"),
            "{out}"
        );
        assert!(
            out.contains("\n  ▌     Choose this path when stakeholders need"),
            "{out}"
        );
        assert!(
            out.contains("\n  ▌     wrapping under the option description indentation."),
            "{out}"
        );
    }

    #[test]
    fn request_id_answer_without_matching_ask_stays_plain() {
        let mut ask = ask_entry("2026-06-28T18:00:00Z", "Native question?");
        ask.request_id = Some(RequestId::new());
        let mut answer = answer_entry("2026-06-28T18:01:00Z", "allow");
        answer.request_id = Some(RequestId::new());

        let out = render(&[ask, answer], jiff::civil::date(2026, 6, 28));

        assert!(
            out.contains("  ▌ Native question?\n  ▌ ◌ unanswered"),
            "{out}"
        );
        assert!(out.contains("you → @claude"), "{out}");
        assert!(out.contains("  allow"), "{out}");
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
