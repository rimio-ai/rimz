//! `rimz asks` — structured reads of actionable agent prompts.

use std::io::Write;

use anyhow::{Result, bail};
use clap::{Args, Subcommand};
use serde::Serialize;

use super::{Ctx, GlobalFlags, ask_commands, ask_pane_owner, resolve_open_ask};
use crate::cli::render;
use rimz::agents::{AgentState, AskKind, OpenAskDetail, read_open_ask};
use rimz::ids::AskId;

#[derive(Debug, Args)]
pub struct AsksArgs {
    #[command(subcommand)]
    command: Option<AsksSubcmd>,
    #[arg(hide = true)]
    target: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
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
    delivery: &'static str,
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
            delivery: view.detail.delivery.label(),
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
    if let Some(target) = args.target {
        let command = super::usage::shell_command(["rimz", "asks", "show", &target]);
        return Err(super::usage::UsageError::new(format!("did you mean `{command}`?")).into());
    }
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
    let peers = rimz::address::addressable_agents(&snapshot);
    let mut views = snapshot
        .agents
        .iter()
        .filter(|agent| {
            all || channel.is_none_or(|channel| agent.channel().as_deref() == Some(channel))
        })
        .flat_map(|agent| agent.actionable_asks().map(move |(ask, _)| (agent, ask.id)))
        .map(|(agent, id)| {
            view_for_agent(store.paths(), agent, &snapshot.agents, &peers, Some(&id))
        })
        .filter_map(Result::transpose)
        .collect::<Result<Vec<_>>>()?;
    views.sort_by_key(|view| view.detail.open.since);

    if json {
        return render::json_pretty(&views.iter().map(AskJsonView::from).collect::<Vec<_>>());
    }
    if views.is_empty() {
        return render::finish(writeln!(
            render::out(),
            "{}",
            render::paint(
                render::palette::faint(),
                &empty_ask_digest(all, ctx.address_context().lane_label().as_deref())
            )
        ));
    }
    let now = jiff::Timestamp::now();
    let mut table = render::Table::new(["AGENT", "KIND", "AGE", "ASK"]);
    for view in views {
        table.row([
            render::cell(view.agent.display_name()),
            render::cell(
                if view.detail.delivery == rimz::agents::AskDelivery::Blocking {
                    view.detail.open.kind.short_label()
                } else {
                    "question (async)"
                },
            ),
            render::cell(render::age_short(view.detail.open.since, now)),
            render::cell(preview(&view)),
        ]);
        table.row([
            render::cell(""),
            render::cell(""),
            render::cell(""),
            render::cell(format!("→ {}", view.commands().next_step())).fg(render::palette::faint()),
        ]);
    }
    let mut out = render::out();
    table.render(&mut out)?;
    render::finish(writeln!(
        out,
        "\nrimz asks show <agent> prints the full prompt"
    ))
}

/// The line a human reads when nothing is blocked. A channel-scoped list hides
/// asks the room may still hold, so it names the flag that widens it; with no
/// current channel the list already covers every channel and `--all` adds
/// nothing.
fn empty_ask_digest(all: bool, channel: Option<&str>) -> String {
    match channel.filter(|_| !all) {
        Some(channel) => {
            format!(
                "no agent in #{channel} is asking anything — rimz asks --all shows every channel"
            )
        }
        None => "no agent is asking anything".to_owned(),
    }
}

fn show(target: &str, json: bool, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let store = &ctx.store;
    let snapshot = ctx.cached_snapshot()?;
    let peers = rimz::address::addressable_agents(&snapshot);
    let agent = resolve_open_ask(&ctx.store, &snapshot, target, &ctx.address_context())?
        .ok_or_else(|| anyhow::anyhow!(ask_commands::unknown_ask(target)))?;
    if agent.actionable_asks().next().is_none() {
        bail!(
            "{} is not asking anything",
            rimz::address::agent_handle(agent, &peers, true)
        );
    }
    let ask_id = target
        .starts_with("ask_")
        .then(|| AskId::parse(target))
        .transpose()?;
    let view = view_for_agent(
        store.paths(),
        agent,
        &snapshot.agents,
        &peers,
        ask_id.as_ref(),
    )?
    .ok_or_else(|| anyhow::anyhow!("ask `{target}` is not actionable or has no live root agent"))?;
    if json {
        return render::json_pretty(&AskJsonView::from(&view));
    }
    let prose = render::prose::Prose::for_stdout();
    let mut out = render::out();
    let now = jiff::Timestamp::now();
    writeln!(
        out,
        "{}  {}  {}",
        render::paint(render::palette::accent(), view.detail.open.id.as_str()),
        view.agent.display_name(),
        render::paint(
            render::palette::muted(),
            &format!(
                "{} · {}",
                if view.detail.delivery == rimz::agents::AskDelivery::Blocking {
                    view.detail.open.kind.short_label()
                } else {
                    "question (async)"
                },
                render::age_short(view.detail.open.since, now)
            )
        )
    )?;
    if let Some(context) = view.detail.context.as_deref() {
        writeln!(out)?;
        for line in prose.lines(context, render::prose::prose_width(2)) {
            if line.is_empty() {
                writeln!(out, "{}", render::paint(render::palette::muted(), "▌"))?;
                continue;
            }
            writeln!(
                out,
                "{} {line}",
                render::paint(render::palette::muted(), "▌")
            )?;
        }
    }
    if view.detail.open.kind == AskKind::Permission {
        writeln!(out, "\nsummary (the pane shows the full tool call):")?;
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
    writeln!(out, "\n{}", view.commands().show_footer())?;
    Ok(())
}

fn view_for_agent(
    paths: &rimz::StatePaths,
    agent: &AgentState,
    agents: &[AgentState],
    peers: &[&AgentState],
    ask_id: Option<&AskId>,
) -> Result<Option<OpenAskView>> {
    let Some(detail) = read_open_ask(paths, agent, ask_id)? else {
        return Ok(None);
    };
    let Some((owner, name)) = ask_pane_owner(agent, agents) else {
        return Ok(None);
    };
    Ok(Some(OpenAskView {
        agent: AskAgentView {
            handle: rimz::address::agent_handle(owner, peers, true),
            name,
            kind: agent.kind.clone(),
            channel: agent.channel(),
        },
        detail,
    }))
}

impl AskAgentView {
    fn display_name(&self) -> String {
        self.name
            .as_deref()
            .map(|name| format!("{name} (via {})", self.handle))
            .unwrap_or_else(|| self.handle.clone())
    }
}

impl OpenAskView {
    fn commands(&self) -> ask_commands::AskCommands<'_> {
        ask_commands::AskCommands {
            detail: &self.detail,
            pane: &self.agent.handle,
            target: if self.agent.name.is_some() {
                self.detail.open.id.as_str()
            } else {
                &self.agent.handle
            },
        }
    }
}

fn preview(view: &OpenAskView) -> String {
    let Some(question) = view.detail.questions.first() else {
        return view
            .detail
            .open
            .detail
            .as_deref()
            .unwrap_or("waiting for input")
            .to_owned();
    };
    let text = ask_commands::question_line(view.detail.open.kind, &question.question);
    if view.detail.questions.len() > 1 {
        return format!("{text} (+{} more)", view.detail.questions.len() - 1);
    }
    // A permission or plan offers one fixed option, which the next-step line names.
    if view.detail.open.kind != AskKind::Question || question.options.is_empty() {
        return text.to_owned();
    }
    let options = question
        .options
        .iter()
        .enumerate()
        .map(|(index, option)| format!("{} {}", index + 1, option.label))
        .collect::<Vec<_>>()
        .join(" · ");
    format!("{text}  {options}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_ask_digest_names_all_only_where_it_widens() {
        assert_eq!(
            empty_ask_digest(false, Some("cli-docs")),
            "no agent in #cli-docs is asking anything — rimz asks --all shows every channel"
        );
        assert_eq!(
            empty_ask_digest(true, Some("cli-docs")),
            "no agent is asking anything"
        );
        assert_eq!(empty_ask_digest(false, None), "no agent is asking anything");
    }
}
