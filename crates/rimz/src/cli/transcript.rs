//! `rimz transcript` — inspect agent and channel conversations from local logs.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Serialize;

use super::{GlobalFlags, current_channel, open_ledger};
use crate::cli::render;
use rimz::agents::AgentState;
use rimz::agents::transcript::{self, AgentChat};
use rimz::feed::{FeedItem, pending_ask_for};
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
struct ChannelAskView {
    agent: String,
    #[serde(flatten)]
    ask: AskView,
}

#[derive(Serialize)]
struct ChatView {
    #[serde(skip_serializing_if = "Option::is_none")]
    channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    focus: Option<String>,
    entries: Vec<rimz::agents::ChatEntry>,
    asks: Vec<ChannelAskView>,
}

struct LoadedTranscript {
    agent: AgentState,
    label: String,
    messages: Vec<rimz::agents::TranscriptMessage>,
}

enum Scope {
    Channel {
        channel: Option<String>,
        agents: Vec<AgentState>,
    },
    Agent {
        channel: Option<String>,
        agents: Vec<AgentState>,
        focus: Box<AgentState>,
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

    let (channel, agents, focus) = match scope {
        Scope::Channel { channel, agents } => (channel, agents, None),
        Scope::Agent {
            channel,
            agents,
            focus,
        } => {
            let include_channel = channel.is_none();
            let focus = rimz::target::agent_handle(&focus, &peers, include_channel);
            (channel, agents, Some(focus))
        }
    };

    let include_channel = channel.is_none();
    let mut loaded = Vec::new();
    for agent in agents {
        let label = rimz::target::agent_handle(&agent, &peers, include_channel);
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
        .map(|loaded| AgentChat {
            handle: loaded.label.clone(),
            messages: loaded.messages.clone(),
        })
        .collect();
    let mut entries =
        transcript::build_chat(per_agent, args.details, rimz::target::parse_sender_prefix);
    let mut asks: Vec<ChannelAskView> = loaded
        .iter()
        .filter_map(|loaded| {
            pending_ask_for(&loaded.agent, feed_items.iter()).map(|ask| ChannelAskView {
                agent: loaded.label.clone(),
                ask: ask_view(ask),
            })
        })
        .collect();

    if let Some(focus) = focus.as_deref() {
        let focus = base_handle(focus);
        entries.retain(|entry| {
            base_handle(&entry.from) == focus
                || entry
                    .to
                    .as_deref()
                    .is_some_and(|to| base_handle(to) == focus)
        });
        asks.retain(|ask| base_handle(&ask.agent) == focus);
    }
    keep_last(&mut entries, args.last);

    if args.json {
        print_json(&ChatView {
            channel,
            focus,
            entries,
            asks,
        })?;
    } else {
        render_chat(channel.as_deref(), &entries, &asks)?;
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
        Some(raw) => {
            let focus = super::resolve_agent_one(snapshot, raw, worktree, current)?.clone();
            let channel = rimz::target::agent_channel(&focus);
            let mut agents = root_agents(snapshot, channel.as_deref());
            if !agents.iter().any(|agent| agent.agent_id == focus.agent_id) {
                agents.push(focus.clone());
            }
            Ok(Scope::Agent {
                channel,
                agents,
                focus: Box::new(focus),
            })
        }
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
    asks: &[ChannelAskView],
) -> Result<()> {
    let mut out = render::out();
    write_header(&mut out, channel)?;
    let grouped = channel.is_some();
    let mut tones = AgentTones::default();
    for entry in entries {
        let from = if entry.from == "user" {
            render::paint(render::palette::COOL, "user")
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
    for ask in asks {
        let display = display_handle(&ask.agent, grouped);
        let handle = render::paint(render::palette::WARN.bold(), display);
        write_chat_line(&mut out, None, &handle, &ask_summary(&ask.ask))?;
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
