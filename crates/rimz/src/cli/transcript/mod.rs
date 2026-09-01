//! `rimz transcript` — inspect agent and channel conversations from local logs.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Write;

use anyhow::{Context, Result, bail};
use clap::Args;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde::Serialize;

use super::{GlobalFlags, current_channel};
use crate::cli::render;
use crate::cli::render::prose::Prose;
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::transcript::{AskOption, TranscriptEntry, TranscriptKind};
use rimz::workspace::WorkspaceResolver;

#[derive(Debug, Args)]
pub struct TranscriptArgs {
    /// Agent address, `#channel`, or `@all`. Omit for the current channel.
    #[arg(add = clap_complete::ArgValueCandidates::new(
        crate::cli::complete::transcript_targets
    ))]
    target: Option<String>,
    /// Override the channel/worktree used to resolve the target.
    #[arg(
        short = 'w',
        long,
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::worktrees)
    )]
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
    /// Render pure timestamp order instead of grouping replies into threads.
    #[arg(long)]
    flat: bool,
}

#[derive(Serialize)]
pub(crate) struct AskView {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub options: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct ChatView {
    #[serde(skip_serializing_if = "Option::is_none")]
    channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    focus: Option<String>,
    pub entries: Vec<ChatLine>,
    #[serde(skip_serializing_if = "Option::is_none")]
    archived_count: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ChatLine {
    pub from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<jiff::Timestamp>,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reply_to: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub error: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<rimz::transcript::AskQuestion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<rimz::transcript::AskAnswer>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

type AgentKey = (AgentKind, AgentSessionId);

#[derive(Clone, Debug)]
pub(crate) struct RenderEntry {
    kind: TranscriptKind,
    agent: AgentKey,
    pub(crate) chat: ChatLine,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hidden {
    Show,
    Skip,
}

impl Hidden {
    pub(crate) fn for_json(json: bool) -> Self {
        if json { Self::Show } else { Self::Skip }
    }

    fn includes(self, entry: &TranscriptEntry) -> bool {
        self == Self::Show || entry.entry != TranscriptKind::SubagentReport
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ViewMode {
    hidden: Hidden,
    flat: bool,
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

mod ask_card;
mod chat;
mod layout;
mod scope;
mod thread;

use chat::{format_marker_when, render_entry_for_log_entry};
use scope::{
    build_identities, compare_optional_timestamps, dedup_asks, entry_in_scope, entry_matches_focus,
    live_boundary, live_root_agents, resolve_scope,
};
use thread::entries_for_view;
#[cfg(test)]
use {chat::*, scope::*, thread::*};
pub fn run(args: TranscriptArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let paths = rimz::StatePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing state paths")?;
    let view = chat_view_with_mode(
        &workspace,
        &paths,
        args.target.as_deref(),
        args.worktree.as_deref(),
        args.last,
        args.all,
        ViewMode {
            hidden: Hidden::for_json(args.json),
            flat: args.flat,
        },
    )?;
    let selected = selected_lines(&view);
    if selected.is_empty() {
        return write_empty_chat(
            args.json,
            view.empty_message
                .as_deref()
                .unwrap_or("No conversation recorded yet."),
        );
    }
    if args.json {
        render::json_pretty(&ChatView {
            channel: view.channel.clone(),
            focus: view.focus.clone(),
            entries: selected,
            archived_count: (view.archived_hidden > 0).then_some(view.archived_hidden),
        })?;
    } else {
        let tz = super::machine_config().time_zone();
        let prose = Prose::for_stdout();
        let mut out = render::out();
        render_lines_to(&mut out, &view, &tz, prose)?;
        if view.archived_hidden > 0 {
            write_archive_hint(view.archived_hidden, view.newest_archived_at, &tz)?;
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub(crate) struct RenderedChat {
    pub(crate) channel: Option<String>,
    pub(crate) focus: Option<String>,
    pub(crate) entries: Vec<RenderEntry>,
    pub(crate) archive_prefix: usize,
    pub(crate) archived_hidden: usize,
    pub(crate) newest_archived_at: Option<jiff::Timestamp>,
    pub(crate) empty_message: Option<String>,
    pub(crate) last: Option<usize>,
    pub(crate) flat: bool,
}

pub(crate) fn chat_view(
    workspace: &rimz::ResolvedWorkspace,
    target: Option<&str>,
    worktree: Option<&str>,
    last: Option<usize>,
    all: bool,
) -> Result<RenderedChat> {
    chat_view_with_hidden(workspace, target, worktree, last, all, Hidden::Skip)
}

pub(crate) fn chat_view_with_hidden(
    workspace: &rimz::ResolvedWorkspace,
    target: Option<&str>,
    worktree: Option<&str>,
    last: Option<usize>,
    all: bool,
    hidden: Hidden,
) -> Result<RenderedChat> {
    let paths = rimz::StatePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing state paths")?;
    chat_view_with_mode(
        workspace,
        &paths,
        target,
        worktree,
        last,
        all,
        ViewMode {
            hidden,
            flat: false,
        },
    )
}

fn chat_view_with_mode(
    workspace: &rimz::ResolvedWorkspace,
    paths: &rimz::StatePaths,
    target: Option<&str>,
    worktree: Option<&str>,
    last: Option<usize>,
    all: bool,
    mode: ViewMode,
) -> Result<RenderedChat> {
    let current = current_channel(workspace);
    let target = resolve_run_target(paths, target)?;
    let entries = dedup_asks(rimz::transcript::read_all(paths)?);
    if entries.is_empty() {
        return Ok(RenderedChat {
            channel: None,
            focus: None,
            entries: Vec::new(),
            archive_prefix: 0,
            archived_hidden: 0,
            newest_archived_at: None,
            empty_message: Some("No conversation recorded yet.".to_owned()),
            last,
            flat: mode.flat,
        });
    }
    let identities = build_identities(&entries);
    let live_agents = live_root_agents(workspace);
    let live_root_keys = live_agents.iter().map(|agent| agent.key.clone()).collect();
    let scope = resolve_scope(
        target.as_deref(),
        worktree,
        current.as_deref(),
        &identities,
        &live_root_keys,
    )?;
    let filtered: Vec<&TranscriptEntry> = entries
        .iter()
        .filter(|entry| entry_in_scope(entry, &scope))
        .filter(|entry| mode.hidden.includes(entry))
        .collect();
    if filtered.is_empty() {
        let empty_message = empty_scope_message(&scope, target.as_deref());
        return Ok(RenderedChat {
            channel: scope.channel,
            focus: scope.focus,
            entries: Vec::new(),
            archive_prefix: 0,
            archived_hidden: 0,
            newest_archived_at: None,
            empty_message: Some(empty_message),
            last,
            flat: mode.flat,
        });
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
        let empty_message = empty_scope_message(&scope, target.as_deref());
        return Ok(RenderedChat {
            channel: scope.channel,
            focus: scope.focus,
            entries: Vec::new(),
            archive_prefix: 0,
            archived_hidden: 0,
            newest_archived_at: None,
            empty_message: Some(empty_message),
            last,
            flat: mode.flat,
        });
    }

    let boundary = live_boundary(&scope, &live_agents);
    let split = match boundary {
        Some(boundary) => {
            entries.partition_point(|entry| entry.chat.at.is_some_and(|at| at < boundary))
        }
        None => entries.len(),
    };
    let show_archive = all || split == entries.len();
    let mut archived_hidden = 0;
    let mut newest_archived_at = None;
    let (shown, archive_prefix) = if show_archive {
        let archive_prefix = archive_prefix(&entries, boundary);
        (entries, archive_prefix)
    } else {
        archived_hidden = split;
        newest_archived_at = split
            .checked_sub(1)
            .and_then(|index| entries[index].chat.at);
        let current = entries.split_off(split);
        (current, 0)
    };

    Ok(RenderedChat {
        channel: scope.channel,
        focus: scope.focus,
        entries: shown,
        archive_prefix,
        archived_hidden,
        newest_archived_at,
        empty_message: None,
        last,
        flat: mode.flat,
    })
}

pub(crate) fn render_lines_to(
    out: &mut impl Write,
    view: &RenderedChat,
    tz: &TimeZone,
    prose: Prose,
) -> Result<()> {
    chat::render_display_chat_to(
        out,
        view.channel.as_deref(),
        &entries_for_view(view),
        tz,
        jiff::Timestamp::now().to_zoned(tz.clone()).date(),
        prose,
    )
}

pub(crate) fn selected_lines(view: &RenderedChat) -> Vec<ChatLine> {
    thread::selected_chat_lines(view)
}

pub(crate) fn render_lines_since_to(
    out: &mut impl Write,
    view: &RenderedChat,
    source_index: usize,
    tz: &TimeZone,
    prose: Prose,
) -> Result<()> {
    let entries = entries_for_view(view)
        .into_iter()
        .filter(|entry| entry.source_index >= source_index)
        .collect::<Vec<_>>();
    chat::render_display_chat_to(
        out,
        view.channel.as_deref(),
        &entries,
        tz,
        jiff::Timestamp::now().to_zoned(tz.clone()).date(),
        prose,
    )
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
        render::json_pretty(&serde_json::json!({ "entries": [] }))?;
    } else {
        let mut out = render::err();
        writeln!(out, "{}", render::paint(render::palette::faint(), message))?;
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

fn resolve_run_target(paths: &rimz::StatePaths, target: Option<&str>) -> Result<Option<String>> {
    let Some(raw) = target else {
        return Ok(None);
    };
    let selector = raw.strip_prefix('@').unwrap_or(raw);
    if selector.contains('#') {
        return Ok(Some(raw.to_owned()));
    }
    let Ok(run_id) = rimz::RunId::parse(selector) else {
        return Ok(Some(raw.to_owned()));
    };
    let record = rimz::harness::run::load(paths, &run_id)?;
    let Some(agent_id) = record.agent_id else {
        bail!("run {run_id} has not bound an agent session yet");
    };
    Ok(Some(agent_id.to_string()))
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
            render::palette::faint(),
            &format!(
                "⋯ {hidden} earlier {line} from a prior session{when} — rimz transcript --all"
            ),
        )
    )?;
    Ok(())
}

pub(crate) fn latest_ask_view(
    workspace: &rimz::ResolvedWorkspace,
    agent: &rimz::agents::AgentState,
) -> Result<Option<AskView>> {
    if !agent.is_awaiting_input() {
        return Ok(None);
    }
    let paths = rimz::StatePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing state paths")?;
    latest_ask_view_from_paths(&paths, agent)
}

fn latest_ask_view_from_paths(
    paths: &rimz::StatePaths,
    agent: &rimz::agents::AgentState,
) -> Result<Option<AskView>> {
    if !agent.is_awaiting_input() {
        return Ok(None);
    }
    rimz::transcript::latest_open_ask(paths, &agent.kind, &agent.agent_id)
        .map(|entry| entry.as_ref().map(ask_view_from_entry))
        .map_err(Into::into)
}

fn ask_view_from_entry(entry: &TranscriptEntry) -> AskView {
    let first_question = entry.questions.first();
    AskView {
        title: first_question
            .map(|question| question.question.clone())
            .or_else(|| (!entry.text.is_empty()).then(|| entry.text.clone()))
            .unwrap_or_else(|| "waiting for input".to_owned()),
        body: (!entry.text.is_empty())
            .then(|| entry.text.clone())
            .filter(|body| Some(body) != first_question.map(|question| &question.question)),
        options: first_question
            .map(|question| {
                question
                    .options
                    .iter()
                    .map(|option| option.label.clone())
                    .collect()
            })
            .unwrap_or_default(),
    }
}

pub(crate) fn ask_summary(ask: &AskView) -> String {
    ask_summary_parts(&ask.title, ask.body.as_deref(), &ask.options)
}

fn ask_summary_parts(title: &str, body: Option<&str>, options: &[String]) -> String {
    let mut text = title.to_owned();
    if let Some(body) = body.map(str::trim).filter(|body| !body.is_empty()) {
        text.push_str(": ");
        text.push_str(body);
    }
    if !options.is_empty() {
        text.push_str(" [");
        text.push_str(&options.join(", "));
        text.push(']');
    }
    text
}

#[cfg(test)]
mod retry_on_error;

#[cfg(test)]
mod tests;
