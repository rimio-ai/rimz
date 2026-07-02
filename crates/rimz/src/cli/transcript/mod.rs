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

const BODY_INDENT: &str = "  ";

mod ask_card;
mod chat;
mod layout;
mod scope;

use chat::{render_chat, render_entry_for_log_entry};
use scope::{
    build_identities, compare_optional_timestamps, dedup_asks, entry_in_scope, entry_matches_focus,
    keep_last, live_root_agent_keys, resolve_scope,
};
#[cfg(test)]
use {chat::*, scope::*};
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

fn print_json(value: &impl Serialize) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}

#[cfg(test)]
mod retry_on_error;

#[cfg(test)]
mod tests;
