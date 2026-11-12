//! `rimz asks` — structured reads of actionable agent prompts.

use std::io::Write;

use anyhow::{Result, bail};
use clap::{Args, Subcommand};
use serde::Serialize;

use super::{Ctx, GlobalFlags, ask_commands, ask_pane_owner, resolve_open_ask};
use crate::cli::render;
use rimz::agents::{AgentState, AskDelivery, AskKind, OpenAskDetail, read_open_ask};
use rimz::ids::AskId;
use rimz::store::raised;

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
    /// Raise a question for yourself or another agent.
    Raise {
        #[arg(value_name = "TARGET|TEXT", allow_hyphen_values = true)]
        first: Option<String>,
        #[arg(value_name = "TEXT", allow_hyphen_values = true)]
        text: Option<String>,
        /// Offer an answer label; repeat for more options.
        #[arg(long = "option", value_name = "LABEL")]
        options: Vec<String>,
    },
    /// Withdraw a raised question (your own when TARGET is omitted).
    Withdraw { target: Option<String> },
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
    raised_by: Option<&'a str>,
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
            raised_by: view.detail.raised_by.as_deref(),
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
        Some(AsksSubcmd::Raise {
            first,
            text,
            options,
        }) => raise(first, text, options, globals),
        Some(AsksSubcmd::Withdraw { target }) => withdraw(target.as_deref(), globals),
        None => list(args.all, args.json, globals),
        Some(AsksSubcmd::List { all, json }) => list(args.all || all, args.json || json, globals),
        Some(AsksSubcmd::Show { target, json }) => show(&target, args.json || json, globals),
    }
}

fn raised_context(globals: &GlobalFlags, target: Option<&str>, verb: &str) -> Result<Ctx> {
    let usage = || {
        super::usage::UsageError::new(format!(
            "from a user shell, name an agent: rimz asks {verb} @<agent>"
        ))
    };
    let ctx = match Ctx::open(globals) {
        Ok(ctx) => ctx,
        Err(_) if target.is_none() && rimz::harness::ancestry::resolve_caller(&[]).is_none() => {
            return Err(usage().into());
        }
        Err(error) => return Err(error),
    };
    if target.is_none() && rimz::launcher::context::resolve_caller(&ctx.store)?.is_none() {
        return Err(usage().into());
    }
    Ok(ctx)
}

fn live_raised_target<'a>(
    ctx: &Ctx,
    snapshot: &'a rimz::store::snapshot::SidebarSnapshot,
    target: Option<&str>,
) -> Result<&'a AgentState> {
    let agent = super::resolve_agent_one_unhinted(
        Some(&ctx.store),
        snapshot,
        target.unwrap_or("@me"),
        None,
        &ctx.address_context(),
    )?;
    if agent.ended_at.is_some() {
        bail!("cannot change raised attention on a closed agent");
    }
    if agent.is_provider_subagent() || agent.agent_id.is_provisional() {
        bail!("raised attention requires a registered agent with its own session");
    }
    Ok(agent)
}

fn raise(
    first: Option<String>,
    text: Option<String>,
    options: Vec<String>,
    globals: &GlobalFlags,
) -> Result<()> {
    let (target, text) = match (first, text) {
        (Some(first), text) if first.starts_with('@') => (Some(first), text),
        (text, None) => (None, text),
        _ => {
            return Err(
                super::usage::UsageError::new("use rimz asks raise @<agent> TEXT".into()).into(),
            );
        }
    };
    if text.as_deref().is_some_and(|text| text.trim().is_empty())
        || options.iter().any(|option| option.trim().is_empty())
    {
        return Err(super::usage::UsageError::new(
            "text and option labels must not be empty".into(),
        )
        .into());
    }
    let ctx = raised_context(globals, target.as_deref(), "raise")?;
    let snapshot = ctx.published_snapshot()?;
    let agent = live_raised_target(&ctx, &snapshot, target.as_deref())?;
    let caller = rimz::launcher::context::resolve_caller(&ctx.store)?;
    let is_self = caller
        .as_ref()
        .and_then(|caller| {
            rimz::harness::ancestry::resolve_launch_caller(&snapshot.agents, caller).ok()
        })
        .is_some_and(|source| source.kind == agent.kind && source.agent_id == agent.agent_id);
    let sender = rimz::launcher::context::sender_for(caller.as_ref(), ctx.channel(), false);
    let actor = caller.as_ref().map(|_| sender.render());
    let ask = raised::prepare_raise(
        ctx.store.paths(),
        agent,
        is_self,
        text,
        options,
        actor.clone(),
        jiff::Timestamp::now(),
    )?;
    let ask_id = ask.value.ask_id.clone();
    raised::raise(ctx.store.paths(), ask)?;
    let mut params = serde_json::json!({"ask_id": ask_id});
    if let Some(actor) = actor {
        params["actor"] = serde_json::json!(actor);
    }
    raised::audit(
        &ctx.store,
        &ctx.workspace.session_name,
        "ask.raised",
        &agent.kind,
        &agent.agent_id,
        params,
    )?;
    let peers = rimz::address::addressable_agents(&snapshot);
    let handle = rimz::address::agent_handle(agent, &peers, true);
    let suffix = if is_self {
        "; the turn's final reply becomes the ask"
    } else {
        ""
    };
    render::finish(writeln!(
        render::out(),
        "raised {ask_id} on {handle}{suffix}"
    ))
}

fn withdraw(target: Option<&str>, globals: &GlobalFlags) -> Result<()> {
    let ctx = raised_context(globals, target, "withdraw")?;
    let snapshot = ctx.cached_snapshot()?;
    let agent = live_raised_target(&ctx, &snapshot, target)?;
    let ask = raised::withdraw(ctx.store.paths(), &agent.kind, &agent.agent_id, None)?
        .ok_or_else(|| anyhow::anyhow!("agent has no raised ask"))?;
    let caller = rimz::launcher::context::resolve_caller(&ctx.store)?;
    let mut params = serde_json::json!({"ask_id": ask.ask_id});
    if caller.is_some() {
        params["actor"] = serde_json::json!(
            rimz::launcher::context::sender_for(caller.as_ref(), ctx.channel(), false).render()
        );
    }
    raised::audit(
        &ctx.store,
        &ctx.workspace.session_name,
        "ask.withdrawn",
        &agent.kind,
        &agent.agent_id,
        params,
    )?;
    let peers = rimz::address::addressable_agents(&snapshot);
    render::finish(writeln!(
        render::out(),
        "withdrew {} from {}",
        ask.ask_id,
        rimz::address::agent_handle(agent, &peers, true)
    ))
}

fn kind_label(detail: &OpenAskDetail) -> &'static str {
    match detail.delivery {
        AskDelivery::Blocking => detail.open.kind.short_label(),
        AskDelivery::Async => "question (async)",
        AskDelivery::Raised => "raised",
    }
}

fn list(all: bool, json: bool, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let store = &ctx.store;
    let snapshot = ctx.cached_snapshot()?;
    let channel = ctx.channel();
    let peers = rimz::address::addressable_agents(&snapshot);
    let visibility_now = snapshot.now;
    let mut views = snapshot
        .agents
        .iter()
        .filter(|agent| {
            all || channel.is_none_or(|channel| agent.lane_name().as_ref() == Some(channel))
        })
        .flat_map(|agent| {
            agent
                .actionable_asks()
                .filter(move |(_, delivery)| {
                    *delivery != AskDelivery::Blocking
                        || agent.attention_ask(visibility_now).is_some()
                })
                .map(move |(ask, _)| (agent, ask.id))
        })
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
            render::cell(kind_label(&view.detail)),
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
    write!(
        out,
        "{}  {}  {}",
        render::paint(render::palette::accent(), view.detail.open.id.as_str()),
        view.agent.display_name(),
        render::paint(
            render::palette::muted(),
            &format!(
                "{} · {}",
                kind_label(&view.detail),
                render::age_short(view.detail.open.since, now)
            )
        )
    )?;
    if let Some(actor) = &view.detail.raised_by {
        write!(out, " · raised by {actor}")?;
    }
    writeln!(out)?;
    if view.detail.delivery == AskDelivery::Raised && view.detail.context.is_none() {
        writeln!(
            out,
            "\nThe reply was not captured; look in the pane: {}",
            view.commands().focus()
        )?;
    }
    if let Some(context) = view.detail.context.as_deref().filter(|context| {
        !(view.detail.delivery == AskDelivery::Raised
            && view.detail.questions.len() == 1
            && context.trim() == view.detail.questions[0].question)
    }) {
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
            channel: agent.lane_name().map(String::from),
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
            target: if self.agent.name.is_some() || self.detail.delivery == AskDelivery::Raised {
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
