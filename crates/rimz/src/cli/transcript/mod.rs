//! `rimz transcript` — inspect agent and channel conversations from local logs.

use std::collections::{BTreeSet, HashMap};
use std::io::Write;

use anyhow::{Context, Result};
use clap::Args;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde::Serialize;
use unicode_width::UnicodeWidthStr;

use super::{GlobalFlags, current_channel};
use crate::cli::render;
use rimz::chat::{AskOption, ChatEntry, ChatKind};
use rimz::feed::FeedItem;
use rimz::ids::{AgentKind, AgentSessionId, RequestId};
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
    /// Include prior-session history archived before the current live cohort.
    #[arg(long)]
    all: bool,
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
    entries: Vec<ChatLine>,
    #[serde(skip_serializing_if = "Option::is_none")]
    archived_count: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct ChatLine {
    pub from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<jiff::Timestamp>,
    pub text: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub error: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<rimz::chat::AskQuestion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<rimz::chat::AskAnswer>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

type AgentKey = (AgentKind, AgentSessionId);

#[derive(Clone, Debug)]
struct RenderEntry {
    kind: ChatKind,
    request_id: Option<RequestId>,
    agent: AgentKey,
    chat: ChatLine,
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

use chat::{format_marker_when, render_chat, render_entry_for_log_entry};
use scope::{
    build_identities, compare_optional_timestamps, dedup_asks, entry_in_scope, entry_matches_focus,
    keep_last, live_boundary, live_root_agents, resolve_scope,
};
#[cfg(test)]
use {chat::*, scope::*};
pub fn run(args: TranscriptArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let paths = rimz::StatePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing state paths")?;
    let current = current_channel(&workspace);
    let entries = dedup_asks(rimz::chat::read_all(&paths)?);
    if entries.is_empty() {
        return write_empty_chat(args.json, "No conversation recorded yet.");
    }
    let identities = build_identities(&entries);
    let live_agents = live_root_agents(&workspace);
    let live_root_keys = live_agents.iter().map(|agent| agent.key.clone()).collect();
    let scope = resolve_scope(
        args.target.as_deref(),
        args.worktree.as_deref(),
        current.as_deref(),
        &identities,
        &live_root_keys,
    )?;
    let filtered: Vec<&ChatEntry> = entries
        .iter()
        .filter(|entry| entry_in_scope(entry, &scope))
        .collect();
    if filtered.is_empty() {
        let message = empty_scope_message(&scope, args.target.as_deref());
        return write_empty_chat(args.json, &message);
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
        let message = empty_scope_message(&scope, args.target.as_deref());
        return write_empty_chat(args.json, &message);
    }

    let boundary = live_boundary(&scope, &live_agents);
    let split = match boundary {
        Some(boundary) => {
            entries.partition_point(|entry| entry.chat.at.is_some_and(|at| at < boundary))
        }
        None => entries.len(),
    };
    let show_archive = args.all || split == entries.len();
    let mut archived_hidden = 0;
    let mut newest_archived_at = None;
    let (shown, archive_prefix) = if show_archive {
        keep_last(&mut entries, args.last);
        let archive_prefix = archive_prefix(&entries, boundary);
        (entries, archive_prefix)
    } else {
        archived_hidden = split;
        newest_archived_at = split
            .checked_sub(1)
            .and_then(|index| entries[index].chat.at);
        let mut current = entries.split_off(split);
        keep_last(&mut current, args.last);
        (current, 0)
    };

    if args.json {
        print_json(&ChatView {
            channel: scope.channel.clone(),
            focus: scope.focus.clone(),
            entries: shown.iter().map(|entry| entry.chat.clone()).collect(),
            archived_count: (archived_hidden > 0).then_some(archived_hidden),
        })?;
    } else {
        let tz = super::machine_config().time_zone();
        render_chat(scope.channel.as_deref(), &shown, archive_prefix, &tz)?;
        if archived_hidden > 0 {
            write_archive_hint(archived_hidden, newest_archived_at, &tz)?;
        }
    }
    Ok(())
}

fn archive_prefix(entries: &[RenderEntry], boundary: Option<jiff::Timestamp>) -> usize {
    match boundary {
        Some(boundary) => entries
            .iter()
            .take_while(|entry| entry.chat.at.is_some_and(|at| at < boundary))
            .count(),
        None => entries.len(),
    }
}

fn write_empty_chat(json: bool, message: &str) -> Result<()> {
    if json {
        print_json(&serde_json::json!({ "entries": [] }))?;
    } else {
        let mut out = render::err();
        writeln!(out, "{}", render::paint(render::palette::FAINT, message))?;
    }
    Ok(())
}

fn empty_scope_message(scope: &Scope, target: Option<&str>) -> String {
    let label = scope
        .channel
        .as_ref()
        .map(|channel| format!("#{channel}"))
        .or_else(|| target.map(ToOwned::to_owned))
        .unwrap_or_else(|| "this room".to_owned());
    format!("No conversation for {label} yet.")
}

fn write_archive_hint(
    hidden: usize,
    newest_archived_at: Option<jiff::Timestamp>,
    tz: &TimeZone,
) -> Result<()> {
    let line = if hidden == 1 { "line" } else { "lines" };
    let when = newest_archived_at
        .map(|at| {
            let today = jiff::Timestamp::now().to_zoned(tz.clone()).date();
            format!(" ({})", format_marker_when(at, tz, today))
        })
        .unwrap_or_default();
    let mut out = render::err();
    writeln!(
        out,
        "{}",
        render::paint(
            render::palette::FAINT,
            &format!(
                "⋯ {hidden} earlier {line} from a prior session{when} — rimz transcript --all"
            ),
        )
    )?;
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
