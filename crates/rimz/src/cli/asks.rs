//! `rimz asks` — structured reads of currently blocking agent prompts.

use std::io::Write;

use anyhow::{Result, bail};
use clap::{Args, Subcommand};
use serde::Serialize;

use super::{Ctx, GlobalFlags, resolve_agent_one};
use crate::cli::render;
use rimz::agents::{AgentState, AskKind, OpenAskDetail, read_open_ask};
use rimz::ids::AskId;

#[derive(Debug, Args)]
pub struct AsksArgs {
    #[command(subcommand)]
    command: Option<AsksSubcmd>,
    /// Include asks from every channel.
    #[arg(long)]
    all: bool,
    /// Emit JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum AsksSubcmd {
    /// List open asks.
    List {
        /// Include asks from every channel.
        #[arg(long)]
        all: bool,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show one open ask by id or agent address.
    Show {
        target: String,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Debug, Serialize)]
struct AskAgentView {
    handle: String,
    kind: rimz::ids::AgentKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel: Option<String>,
}

#[derive(Clone, Debug)]
struct OpenAskView {
    agent: AskAgentView,
    detail: OpenAskDetail,
}

#[derive(Serialize)]
struct AskJsonView<'a> {
    ask_id: &'a AskId,
    agent: &'a AskAgentView,
    kind: AskKind,
    since: jiff::Timestamp,
    detail: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<&'a str>,
    questions: Vec<AskQuestionJson<'a>>,
}

#[derive(Serialize)]
struct AskQuestionJson<'a> {
    question: &'a str,
    options: Vec<AskOptionJson<'a>>,
    multi_select: bool,
}

#[derive(Serialize)]
struct AskOptionJson<'a> {
    label: &'a str,
    description: Option<&'a str>,
    mutates_trust: bool,
    caution: Option<&'a str>,
}

impl<'a> From<&'a OpenAskView> for AskJsonView<'a> {
    fn from(view: &'a OpenAskView) -> Self {
        Self {
            ask_id: &view.detail.open.id,
            agent: &view.agent,
            kind: view.detail.open.kind,
            since: view.detail.open.since,
            detail: view.detail.open.detail.as_deref(),
            context: view.detail.context.as_deref(),
            questions: view
                .detail
                .questions
                .iter()
                .map(|question| AskQuestionJson {
                    question: &question.question,
                    options: question
                        .options
                        .iter()
                        .map(|option| AskOptionJson {
                            label: &option.label,
                            description: option.description.as_deref(),
                            mutates_trust: option.caution.is_some(),
                            caution: option.caution.as_deref(),
                        })
                        .collect(),
                    multi_select: question.multi_select,
                })
                .collect(),
        }
    }
}

pub fn run(args: AsksArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        None => list(args.all, args.json, globals),
        Some(AsksSubcmd::List { all, json }) => list(args.all || all, args.json || json, globals),
        Some(AsksSubcmd::Show { target, json }) => show(&target, args.json || json, globals),
    }
}

fn list(all: bool, json: bool, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let store = &ctx.store;
    let snapshot = ctx.cached_snapshot()?;
    let channel = ctx.channel();
    let peers = root_peers(&snapshot);
    let mut views = snapshot
        .agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .filter(|agent| agent.is_awaiting_input() && agent.open_ask.is_some())
        .filter(|agent| {
            all || channel.is_none_or(|channel| {
                rimz::harness::target::agent_channel(agent).as_deref() == Some(channel)
            })
        })
        .map(|agent| view_for_agent(store.paths(), agent, &peers))
        .collect::<Result<Vec<_>>>()?;
    views.sort_by_key(|view| view.detail.open.since);

    if json {
        return render::json_pretty(&views.iter().map(AskJsonView::from).collect::<Vec<_>>());
    }
    if views.is_empty() {
        return Ok(());
    }
    let now = jiff::Timestamp::now();
    let mut table = render::Table::new(["ASK", "AGENT", "KIND", "AGE", "QUESTION"]);
    for view in views {
        let question = first_line(&view).to_owned();
        table.row([
            render::cell(view.detail.open.id.as_str()).fg(render::palette::accent()),
            render::cell(view.agent.handle.as_str()),
            render::cell(view.detail.open.kind.short_label()),
            render::cell(render::age_short(view.detail.open.since, now)),
            render::cell(question),
        ]);
    }
    render::finish(table.render(&mut render::out()))
}

fn show(target: &str, json: bool, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let store = &ctx.store;
    let snapshot = ctx.cached_snapshot()?;
    let peers = root_peers(&snapshot);
    let agent = if target.starts_with("ask_") {
        let ask_id = AskId::parse(target)?;
        snapshot
            .agents
            .iter()
            .find(|agent| {
                agent.parent_agent_id.is_none()
                    && agent.is_awaiting_input()
                    && agent.open_ask.as_ref().is_some_and(|ask| ask.id == ask_id)
            })
            .ok_or_else(|| anyhow::anyhow!("ask `{ask_id}` is no longer open"))?
    } else {
        resolve_agent_one(&snapshot, target, None, ctx.channel())?
    };
    if !agent.is_awaiting_input() || agent.open_ask.is_none() {
        bail!(
            "{} is not asking anything",
            rimz::harness::target::agent_handle(agent, &peers, true)
        );
    }
    let view = view_for_agent(store.paths(), agent, &peers)?;
    if json {
        return render::json_pretty(&AskJsonView::from(&view));
    }
    let mut out = render::out();
    let now = jiff::Timestamp::now();
    writeln!(
        out,
        "{}  {}  {}",
        render::paint(render::palette::accent(), view.detail.open.id.as_str()),
        view.agent.handle,
        render::paint(
            render::palette::muted(),
            &format!(
                "{} · {}",
                view.detail.open.kind.short_label(),
                render::age_short(view.detail.open.since, now)
            )
        )
    )?;
    if let Some(context) = view.detail.context.as_deref() {
        writeln!(out)?;
        let width = render::terminal_columns(100)
            .min(100)
            .saturating_sub(2)
            .max(1);
        for source_line in context.lines() {
            let lines = render::wrap_words(source_line, width);
            if lines.is_empty() {
                writeln!(out, "{}", render::paint(render::palette::muted(), "▌"))?;
            } else {
                for line in lines {
                    writeln!(
                        out,
                        "{}",
                        render::paint(render::palette::muted(), &format!("▌ {line}"))
                    )?;
                }
            }
        }
    }
    for (question_index, question) in view.detail.questions.iter().enumerate() {
        if view.detail.questions.len() > 1 {
            writeln!(out, "\n{}. {}", question_index + 1, question.question)?;
        } else {
            writeln!(out, "\n{}", question.question)?;
        }
        for (option_index, option) in question.options.iter().enumerate() {
            let guard = option
                .caution
                .as_deref()
                .map(|caution| format!(" [caution: {caution}]"))
                .unwrap_or_default();
            writeln!(out, "  {}. {}{}", option_index + 1, option.label, guard)?;
            if let Some(description) = option.description.as_deref() {
                writeln!(out, "     {description}")?;
            }
        }
    }
    Ok(())
}

fn view_for_agent(
    paths: &rimz::StatePaths,
    agent: &AgentState,
    peers: &[&AgentState],
) -> Result<OpenAskView> {
    let detail = read_open_ask(paths, agent)?
        .ok_or_else(|| anyhow::anyhow!("agent is not asking anything"))?;
    Ok(OpenAskView {
        agent: AskAgentView {
            handle: rimz::harness::target::agent_handle(agent, peers, true),
            kind: agent.kind.clone(),
            channel: rimz::harness::target::agent_channel(agent),
        },
        detail,
    })
}

fn root_peers(snapshot: &rimz::SidebarSnapshot) -> Vec<&AgentState> {
    snapshot
        .agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .collect()
}

fn first_line(view: &OpenAskView) -> &str {
    view.detail
        .questions
        .first()
        .map(|question| question.question.lines().next().unwrap_or_default())
        .or(view.detail.open.detail.as_deref())
        .unwrap_or("waiting for input")
}
