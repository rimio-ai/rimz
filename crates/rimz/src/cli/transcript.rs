//! `rimz transcript` — inspect agent and channel conversations from local logs.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;

use anyhow::{Context, Result, bail};
use clap::Args;
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
    focus_base: Option<String>,
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
        match entry.entry {
            TranscriptKind::Prompt | TranscriptKind::Assistant => {
                per_agent_messages
                    .entry(entry_key(entry))
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
            TranscriptKind::Ask => direct.push(rimz::agents::ChatEntry {
                from: handle_for(entry, &identities, scope.include_channel),
                to: None,
                at: Some(entry.at),
                text: entry.text.clone(),
            }),
            TranscriptKind::Answer => direct.push(rimz::agents::ChatEntry {
                from: entry.from.clone().unwrap_or_else(|| "resolver".to_owned()),
                to: Some(handle_for(entry, &identities, scope.include_channel)),
                at: Some(entry.at),
                text: entry.text.clone(),
            }),
        }
    }

    let per_agent = per_agent_messages
        .into_iter()
        .map(|(key, messages)| AgentChat {
            handle: handle_for_key(&key, &identities, scope.include_channel),
            messages,
        })
        .collect();
    let mut entries =
        transcript::build_chat(per_agent, args.details, rimz::target::parse_sender_prefix);
    entries.extend(direct);
    entries.sort_by(|left, right| match (left.at, right.at) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });

    if let Some(focus) = scope.focus_base.as_deref() {
        entries.retain(|entry| {
            base_handle(&entry.from) == focus
                || entry
                    .to
                    .as_deref()
                    .is_some_and(|to| base_handle(to) == focus)
        });
    }
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
        render_chat(scope.channel.as_deref(), &entries)?;
    }
    Ok(())
}

fn dedup_asks(entries: Vec<TranscriptEntry>) -> Vec<TranscriptEntry> {
    let mut other = Vec::new();
    let mut latest_asks: HashMap<RequestId, TranscriptEntry> = HashMap::new();
    for entry in entries {
        if entry.entry == TranscriptKind::Ask
            && let Some(request_id) = entry.request_id.clone()
        {
            latest_asks
                .entry(request_id)
                .and_modify(|prior| {
                    if entry.at >= prior.at {
                        *prior = entry.clone();
                    }
                })
                .or_insert(entry);
        } else {
            other.push(entry);
        }
    }
    other.extend(latest_asks.into_values());
    other
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
                focus_base: None,
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
                focus_base: None,
                include_channel,
            })
        }
        Some(raw) => {
            let (selector, inline) = split_agent_target(raw)?;
            let explicit_or_current = reconcile_channel(raw, inline, worktree, current)?;
            let matches = matching_identities(selector, explicit_or_current.as_deref(), identities);
            let Some((_, identity)) = matches.first() else {
                bail!("no agent matches target `{raw}` in the transcript log");
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
                focus_base: Some(identity.base_handle.clone()),
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
        focus_base: None,
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
    identities
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
        .collect()
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

fn render_chat(channel: Option<&str>, entries: &[rimz::agents::ChatEntry]) -> Result<()> {
    let mut out = render::out();
    write_header(&mut out, channel)?;
    let grouped = channel.is_some();
    let mut tones = AgentTones::default();
    for entry in entries {
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
        write_chat_line(&mut out, entry.at, &from, &format!("{to}{}", entry.text))?;
    }
    Ok(())
}

fn write_chat_line(
    out: &mut impl Write,
    at: Option<jiff::Timestamp>,
    from: &str,
    text: &str,
) -> Result<()> {
    let time = at.map_or_else(
        || "        ".to_owned(),
        |at| at.strftime("%H:%M:%S").to_string(),
    );
    let time = render::paint(render::palette::FAINT, &time);
    let mut lines = text.lines();
    match lines.next() {
        Some(first) => writeln!(out, "{time} {from}: {first}")?,
        None => writeln!(out, "{time} {from}:")?,
    }
    let padding = render::paint(render::palette::FAINT, "        ");
    for line in lines {
        writeln!(out, "{padding}   {line}")?;
    }
    Ok(())
}

fn print_json(value: &impl Serialize) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}
