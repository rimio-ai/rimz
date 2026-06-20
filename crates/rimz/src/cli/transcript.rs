//! `rimz transcript` — inspect agent and channel conversations from local logs.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::Args;
use jiff::Timestamp;
use serde::Serialize;

use super::{GlobalFlags, current_channel, open_ledger};
use crate::cli::render;
use rimz::agents::transcript::{TranscriptRole, fuse_timeline, group_turns};
use rimz::feed::{AgentState, FeedItem, pending_ask_for};
use rimz::workspace::WorkspaceResolver;

#[derive(Debug, Args)]
pub struct TranscriptArgs {
    /// Agent address, `#channel`, or `@all`. Omit for the current channel.
    target: Option<String>,
    /// Override the channel/worktree used to resolve the target.
    #[arg(short = 'w', long)]
    worktree: Option<String>,
    /// Keep the last N turns for one agent, or last N entries for a channel.
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
struct AgentTranscriptView {
    agent: String,
    turns: Vec<rimz::agents::Turn>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ask: Option<AskView>,
}

#[derive(Serialize)]
struct ChannelAskView {
    agent: String,
    #[serde(flatten)]
    ask: AskView,
}

#[derive(Serialize)]
struct ChannelTranscriptView {
    #[serde(skip_serializing_if = "Option::is_none")]
    channel: Option<String>,
    timeline: Vec<rimz::agents::TimelineEntry>,
    asks: Vec<ChannelAskView>,
}

struct LoadedTranscript {
    agent: AgentState,
    label: String,
    messages: Vec<rimz::agents::TranscriptMessage>,
}

enum Scope {
    Agent(Box<AgentState>),
    Channel {
        channel: Option<String>,
        agents: Vec<AgentState>,
    },
}

pub fn run(args: TranscriptArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let ledger = open_ledger(&workspace)?;
    let runtime = rimz::RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    let snapshot = ledger
        .snapshot_cached()
        .context("reading agent snapshot")?
        .with_agent_context(rimz::ledger::agent_context::read_all(&runtime));
    let current = current_channel(&workspace);
    let scope = resolve_scope(
        &snapshot,
        args.target.as_deref(),
        args.worktree.as_deref(),
        current.as_deref(),
    )?;
    let feed_items = ledger.list_feed_items()?;
    let peers: Vec<&AgentState> = snapshot
        .agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .collect();

    match scope {
        Scope::Agent(agent) => {
            let label = rimz::target::agent_handle(&agent, &peers, true);
            let messages = load_agent_messages(&agent)?
                .with_context(|| format!("no transcript on disk for {label} yet"))?;
            let mut turns = group_turns(&messages, args.details);
            keep_last(&mut turns, args.last);
            let ask = pending_ask_for(&agent, feed_items.iter()).map(ask_view);
            if args.json {
                print_json(&AgentTranscriptView {
                    agent: label,
                    turns,
                    ask,
                })?;
            } else {
                let channel = rimz::target::agent_channel(&agent);
                render_agent(channel.as_deref(), &label, &turns, ask.as_ref())?;
            }
        }
        Scope::Channel { channel, agents } => {
            let mut loaded = Vec::new();
            for agent in agents {
                let label = rimz::target::agent_handle(&agent, &peers, true);
                if let Some(messages) = load_agent_messages(&agent)? {
                    loaded.push(LoadedTranscript {
                        agent,
                        label,
                        messages,
                    });
                } else if pending_ask_for(&agent, feed_items.iter()).is_some() {
                    loaded.push(LoadedTranscript {
                        agent,
                        label,
                        messages: Vec::new(),
                    });
                }
            }
            if loaded.is_empty() {
                bail!("no transcripts on disk for this scope yet");
            }
            let per_agent = loaded
                .iter()
                .map(|loaded| (loaded.label.clone(), loaded.messages.clone()))
                .collect();
            let mut timeline = fuse_timeline(per_agent, args.details);
            keep_last(&mut timeline, args.last);
            let asks: Vec<ChannelAskView> = loaded
                .iter()
                .filter_map(|loaded| {
                    pending_ask_for(&loaded.agent, feed_items.iter()).map(|ask| ChannelAskView {
                        agent: loaded.label.clone(),
                        ask: ask_view(ask),
                    })
                })
                .collect();
            if args.json {
                print_json(&ChannelTranscriptView {
                    channel,
                    timeline,
                    asks,
                })?;
            } else {
                render_channel(channel.as_deref(), &timeline, &asks)?;
            }
        }
    }
    Ok(())
}

fn resolve_scope(
    snapshot: &rimz::SidebarSnapshot,
    target: Option<&str>,
    worktree: Option<&str>,
    current: Option<&str>,
) -> Result<Scope> {
    match target {
        None => {
            let channel = worktree.or(current).map(ToOwned::to_owned);
            let agents = root_agents(snapshot, channel.as_deref());
            if agents.is_empty() {
                bail!(empty_channel_message(channel.as_deref()));
            }
            Ok(Scope::Channel { channel, agents })
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
            let agents = root_agents(snapshot, Some(channel));
            if agents.is_empty() {
                bail!("no agents in channel `#{channel}`");
            }
            Ok(Scope::Channel {
                channel: Some(channel.to_owned()),
                agents,
            })
        }
        Some(raw) if raw == "@all" || raw.starts_with("@all#") => {
            let agents = super::resolve_agent_many(snapshot, raw, worktree, current)?
                .into_iter()
                .cloned()
                .collect();
            Ok(Scope::Channel {
                channel: channel_label(raw, worktree, current),
                agents,
            })
        }
        Some(raw) => Ok(Scope::Agent(Box::new(
            super::resolve_agent_one(snapshot, raw, worktree, current)?.clone(),
        ))),
    }
}

fn root_agents(snapshot: &rimz::SidebarSnapshot, channel: Option<&str>) -> Vec<AgentState> {
    snapshot
        .agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .filter(|agent| channel.is_none_or(|filter| rimz::target::agent_in_worktree(agent, filter)))
        .cloned()
        .collect()
}

fn channel_label(raw: &str, worktree: Option<&str>, current: Option<&str>) -> Option<String> {
    raw.split_once('#')
        .map(|(_, channel)| channel.to_owned())
        .or_else(|| worktree.or(current).map(ToOwned::to_owned))
}

fn empty_channel_message(channel: Option<&str>) -> String {
    match channel {
        Some(channel) => format!("no agents in channel `#{channel}`"),
        None => "no agents in this workspace".to_owned(),
    }
}

fn load_agent_messages(agent: &AgentState) -> Result<Option<Vec<rimz::agents::TranscriptMessage>>> {
    let Some(adapter) = rimz::agents::find_adapter(agent.kind.as_str()) else {
        bail!("unknown agent integration `{}`", agent.kind);
    };
    let prior = agent.transcript_path.as_deref().map(Path::new);
    let Some(path) = adapter.session_transcript(agent.agent_id.as_str(), prior) else {
        return Ok(None);
    };
    let Some((bytes, _)) = rimz::agents::read_transcript_lines(&path, 0) else {
        return Ok(None);
    };
    let text = String::from_utf8_lossy(&bytes);
    Ok(Some(adapter.parse_transcript_messages(&text)))
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
    let mut text = ask.title.clone();
    if let Some(body) = ask
        .body
        .as_deref()
        .map(str::trim)
        .filter(|body| !body.is_empty())
    {
        text.push_str(": ");
        text.push_str(body);
    }
    if !ask.options.is_empty() {
        text.push_str(" [");
        text.push_str(&ask.options.join(", "));
        text.push(']');
    }
    text
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

fn render_agent(
    channel: Option<&str>,
    label: &str,
    turns: &[rimz::agents::Turn],
    ask: Option<&AskView>,
) -> Result<()> {
    let mut out = render::out();
    write_header(&mut out, channel)?;
    let display = if channel.is_some() {
        base_handle(label)
    } else {
        label
    };
    let agent = render::paint(render::palette::META.bold(), display);
    let user = render::paint(render::palette::COOL, "user");
    let assistant = render::paint(render::palette::META, "assistant");
    let ask_role = render::paint(render::palette::WARN, "ask");
    let mut first = true;
    for turn in turns {
        for message in &turn.messages {
            if !first {
                writeln!(out)?;
            }
            first = false;
            match message.role {
                TranscriptRole::User => {
                    write_block(&mut out, &user, message.at, None, &message.text)?;
                }
                TranscriptRole::Assistant => {
                    write_block(
                        &mut out,
                        &assistant,
                        message.at,
                        Some(&agent),
                        &message.text,
                    )?;
                }
            }
        }
    }
    if let Some(ask) = ask {
        if !first {
            writeln!(out)?;
        }
        write_block(&mut out, &ask_role, None, None, &ask_summary(ask))?;
    }
    Ok(())
}

fn render_channel(
    channel: Option<&str>,
    timeline: &[rimz::agents::TimelineEntry],
    asks: &[ChannelAskView],
) -> Result<()> {
    let mut out = render::out();
    write_header(&mut out, channel)?;
    let grouped = channel.is_some();
    let mut tones = AgentTones::default();
    let user = render::paint(render::palette::COOL, "user");
    let assistant = render::paint(render::palette::META, "assistant");
    let ask_role = render::paint(render::palette::WARN, "ask");
    let mut first = true;
    for entry in timeline {
        if !first {
            writeln!(out)?;
        }
        first = false;
        let display = if grouped {
            base_handle(&entry.agent)
        } else {
            entry.agent.as_str()
        };
        match entry.role {
            TranscriptRole::User => write_block(&mut out, &user, entry.at, None, &entry.text)?,
            TranscriptRole::Assistant => {
                let handle = render::paint(tones.tone(display).bold(), display);
                write_block(&mut out, &assistant, entry.at, Some(&handle), &entry.text)?;
            }
        }
    }
    for ask in asks {
        if !first {
            writeln!(out)?;
        }
        first = false;
        let display = if grouped {
            base_handle(&ask.agent)
        } else {
            ask.agent.as_str()
        };
        let handle = render::paint(tones.tone(display).bold(), display);
        write_block(
            &mut out,
            &ask_role,
            None,
            Some(&handle),
            &ask_summary(&ask.ask),
        )?;
    }
    Ok(())
}

fn write_block(
    out: &mut impl Write,
    role: &str,
    at: Option<Timestamp>,
    handle: Option<&str>,
    text: &str,
) -> Result<()> {
    match at {
        Some(at) => writeln!(
            out,
            "{role}  {}",
            render::paint(render::palette::FAINT, &at.strftime("%H:%M:%S").to_string())
        )?,
        None => writeln!(out, "{role}")?,
    }

    let mut lines = text.lines();
    match (handle, lines.next()) {
        (Some(handle), Some(first)) => writeln!(out, "  {handle}  {first}")?,
        (Some(handle), None) => writeln!(out, "  {handle}")?,
        (None, Some(first)) => writeln!(out, "  {first}")?,
        (None, None) => {}
    }
    for line in lines {
        writeln!(out, "  {line}")?;
    }
    Ok(())
}

fn print_json(value: &impl Serialize) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}
