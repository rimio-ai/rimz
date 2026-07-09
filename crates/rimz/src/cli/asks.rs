//! `rimz asks` — structured reads of currently blocking agent prompts.

use std::io::Write;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde::Serialize;

use super::{GlobalFlags, current_channel, open_store, resolve_agent_one};
use crate::cli::render;
use rimz::agents::{AgentState, AskKind};
use rimz::ids::AskId;
use rimz::transcript::AskQuestion;
use rimz::workspace::WorkspaceResolver;

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
pub(crate) struct AskAgentView {
    pub handle: String,
    pub kind: rimz::ids::AgentKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct OpenAskView {
    pub ask_id: AskId,
    pub agent: AskAgentView,
    pub kind: AskKind,
    pub since: jiff::Timestamp,
    pub detail: Option<String>,
    pub questions: Vec<AskQuestion>,
}

#[derive(Serialize)]
struct AskJsonView<'a> {
    ask_id: &'a AskId,
    agent: &'a AskAgentView,
    kind: AskKind,
    since: jiff::Timestamp,
    detail: Option<&'a str>,
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
            ask_id: &view.ask_id,
            agent: &view.agent,
            kind: view.kind,
            since: view.since,
            detail: view.detail.as_deref(),
            questions: view
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
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let store = open_store(&workspace)?;
    let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
    let channel = current_channel(&workspace);
    let peers = root_peers(&snapshot);
    let mut views = snapshot
        .agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .filter(|agent| agent.is_awaiting_input() && agent.open_ask.is_some())
        .filter(|agent| {
            all || channel.as_deref().is_none_or(|channel| {
                rimz::harness::target::agent_channel(agent).as_deref() == Some(channel)
            })
        })
        .map(|agent| view_for_agent(&workspace, agent, &peers))
        .collect::<Result<Vec<_>>>()?;
    views.sort_by_key(|view| view.since);

    if json {
        return print_json(&views.iter().map(AskJsonView::from).collect::<Vec<_>>());
    }
    if views.is_empty() {
        return Ok(());
    }
    let now = jiff::Timestamp::now();
    let mut table = render::Table::new(["ASK", "AGENT", "KIND", "AGE", "QUESTION"]);
    for view in views {
        let question = first_line(&view).to_owned();
        table.row([
            render::cell(view.ask_id.as_str()).fg(render::palette::ACCENT),
            render::cell(view.agent.handle.as_str()),
            render::cell(ask_kind_label(view.kind)),
            render::cell(render::age_short(view.since, now)),
            render::cell(question),
        ]);
    }
    render::finish(table.render(&mut render::out()))
}

fn show(target: &str, json: bool, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let store = open_store(&workspace)?;
    let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
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
        let channel = current_channel(&workspace);
        resolve_agent_one(&snapshot, target, None, channel.as_deref())?
    };
    if !agent.is_awaiting_input() || agent.open_ask.is_none() {
        bail!(
            "{} is not asking anything",
            rimz::harness::target::agent_handle(agent, &peers, true)
        );
    }
    let view = view_for_agent(&workspace, agent, &peers)?;
    if json {
        return print_json(&AskJsonView::from(&view));
    }
    let mut out = render::out();
    writeln!(out, "{}  {}", view.ask_id, view.agent.handle)?;
    if let Some(detail) = view.detail.as_deref() {
        writeln!(out, "{detail}")?;
    }
    for (question_index, question) in view.questions.iter().enumerate() {
        if view.questions.len() > 1 {
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

pub(crate) fn view_for_agent(
    workspace: &rimz::ResolvedWorkspace,
    agent: &AgentState,
    peers: &[&AgentState],
) -> Result<OpenAskView> {
    let open = agent
        .open_ask
        .as_ref()
        .filter(|_| agent.is_awaiting_input())
        .ok_or_else(|| anyhow::anyhow!("agent is not asking anything"))?;
    let adapter = rimz::agents::adapter_by_kind(agent.kind.as_str())?;
    let paths = rimz::StatePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing transcript paths")?;
    let questions = match open.kind {
        AskKind::Question | AskKind::PlanApproval => {
            rimz::transcript::latest_open_ask(&paths, &agent.kind, &agent.agent_id)?
                .filter(|entry| entry.id.as_ref() == Some(&open.id))
                .map(|entry| entry.questions)
                .unwrap_or_else(|| synthetic_questions(adapter, open.kind, open.detail.as_deref()))
        }
        AskKind::Permission => synthetic_questions(adapter, open.kind, open.detail.as_deref()),
    };
    Ok(OpenAskView {
        ask_id: open.id.clone(),
        agent: AskAgentView {
            handle: rimz::harness::target::agent_handle(agent, peers, true),
            kind: agent.kind.clone(),
            channel: rimz::harness::target::agent_channel(agent),
        },
        kind: open.kind,
        since: open.since,
        detail: open.detail.clone(),
        questions,
    })
}

fn synthetic_questions(
    adapter: &dyn rimz::agents::AgentAdapter,
    kind: AskKind,
    detail: Option<&str>,
) -> Vec<AskQuestion> {
    vec![AskQuestion {
        question: detail.unwrap_or_else(|| ask_kind_label(kind)).to_owned(),
        options: adapter.ask_options(kind).unwrap_or_default(),
        multi_select: false,
        has_option_previews: false,
    }]
}

fn root_peers(snapshot: &rimz::SidebarSnapshot) -> Vec<&AgentState> {
    snapshot
        .agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .collect()
}

fn ask_kind_label(kind: AskKind) -> &'static str {
    match kind {
        AskKind::Permission => "permission",
        AskKind::PlanApproval => "plan approval",
        AskKind::Question => "question",
    }
}

fn first_line(view: &OpenAskView) -> &str {
    view.questions
        .first()
        .map(|question| question.question.lines().next().unwrap_or_default())
        .or(view.detail.as_deref())
        .unwrap_or("waiting for input")
}

fn print_json(value: &impl Serialize) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    serde_json::to_writer_pretty(&mut out, value)?;
    writeln!(out)?;
    Ok(())
}
