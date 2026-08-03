//! `rimz answer` — validate and drive one current native prompt atomically.

use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Args;
use serde::Deserialize;

use super::{Ctx, GlobalFlags, resolve_open_ask};
use rimz::agents::{AnswerPlanErr, AnswerStep, AskKind, AskReply};
use rimz::ids::AskId;
use rimz::mux::{paste_into_pane, press_pane_key, type_into_pane};
use rimz::transcript::{AskAnswer, AskQuestion, TranscriptEntry, TranscriptKind};

const DEFAULT_ANSWER_WAIT: Duration = Duration::from_secs(30);
const CONFIRM_POLL: Duration = Duration::from_millis(100);

#[derive(Debug, Args)]
pub struct AnswerArgs {
    /// Current ask id or agent address.
    target: String,
    /// One selector per question. Commas select several options.
    selectors: Vec<String>,
    /// Free-text answer for a single question.
    #[arg(long)]
    text: Option<String>,
    /// Read structured answers from FILE, or stdin when FILE is omitted.
    #[arg(long, value_name = "FILE", num_args = 0..=1)]
    json: Option<Option<PathBuf>>,
    /// Confirmation timeout.
    #[arg(long, value_name = "DURATION", value_parser = parse_wait, conflicts_with = "no_wait")]
    wait: Option<Duration>,
    /// Return after sending without waiting for lifecycle confirmation.
    #[arg(long)]
    no_wait: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct JsonAnswer {
    #[serde(default)]
    pick: Vec<JsonPick>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum JsonPick {
    Index(usize),
    Label(String),
}

pub fn run(args: AnswerArgs, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let store = &ctx.store;
    let snapshot = ctx.cached_snapshot()?;
    let peers = rimz::harness::target::addressable_agents(&snapshot);
    let agent = resolve_current_agent(&snapshot, &args.target, ctx.channel())
        .unwrap_or_else(|message| answer_exit(2, &message));
    let detail = rimz::agents::read_open_ask(store.paths(), agent)
        .unwrap_or_else(|err| answer_exit(2, &err.to_string()))
        .unwrap_or_else(|| answer_exit(2, "agent is not asking anything"));
    let ask_id = detail.open.id.clone();
    let kind = agent.kind.clone();
    let agent_id = agent.agent_id.clone();
    let handle = rimz::harness::target::agent_handle(agent, &peers, true);
    let adapter = rimz::agents::definition_by_kind(kind.as_str())
        .unwrap_or_else(|err| answer_exit(3, &err.to_string()));
    if let Err(AnswerPlanErr::Unsupported(kind)) =
        adapter.answer_plan(detail.open.kind, &detail.questions, &[])
    {
        answer_exit(3, &format!("{kind} does not support structured answers"));
    }
    let replies = parse_replies(&args, detail.open.kind, &detail.questions)
        .unwrap_or_else(|message| answer_exit(3, &message));
    let steps = adapter
        .answer_plan(detail.open.kind, &detail.questions, &replies)
        .unwrap_or_else(|err| answer_exit(3, &err.to_string()));

    // Re-read immediately before the first keystroke. This is the compare half
    // of the ask-id CAS; a prompt answered or superseded during validation gets
    // no input from this command.
    let current = store.snapshot_cached().context("rechecking current ask")?;
    let still_current = current.agents.iter().any(|agent| {
        agent.kind == kind
            && agent.agent_id == agent_id
            && agent.is_awaiting_input()
            && agent.open_ask.as_ref().is_some_and(|ask| ask.id == ask_id)
    });
    if !still_current {
        answer_exit(2, &format!("ask `{ask_id}` is no longer current"));
    }

    let live = ctx.resolution_snapshot()?;
    let target = live
        .agent_panes
        .iter()
        .find(|pane| pane.kind == kind && pane.agent_id.as_ref().is_some_and(|id| id == &agent_id))
        .unwrap_or_else(|| answer_exit(2, &format!("{handle} has no live bound pane")));
    let mut pacer = rimz::message::send::Pacer::new(rimz::message::message_interval_from_env());
    for step in steps {
        pacer.tick();
        let result = match step {
            AnswerStep::Text(text) => type_into_pane(&target.pane_id, &text),
            AnswerStep::Key(key) => press_pane_key(&target.pane_id, key),
            AnswerStep::Paste(text) => paste_into_pane(&target.pane_id, &text),
        };
        if let Err(err) = result {
            answer_exit(2, &format!("sending answer to {handle}: {err}"));
        }
    }

    if args.no_wait {
        let mut out = super::render::out();
        writeln!(out, "sent answer for {ask_id} to {handle}")?;
        return Ok(());
    }

    let wait = args.wait.unwrap_or(DEFAULT_ANSWER_WAIT);
    if !wait_for_confirmation(store, &kind, &agent_id, &ask_id, wait)? {
        answer_exit(
            4,
            &format!(
                "answer sent for `{ask_id}`, but the agent did not confirm it within {wait:?}"
            ),
        );
    }
    record_answer_if_missing(store, agent, &ask_id, &detail.questions, &replies)?;
    let mut out = super::render::out();
    writeln!(out, "answered {ask_id} for {handle}")?;
    Ok(())
}

fn resolve_current_agent<'a>(
    snapshot: &'a rimz::store::snapshot::SidebarSnapshot,
    target: &str,
    channel: Option<&str>,
) -> std::result::Result<&'a rimz::agents::AgentState, String> {
    let agent = resolve_open_ask(snapshot, target, channel, false)
        .map_err(|err| err.to_string())?
        .ok_or_else(|| format!("ask `{target}` is no longer current"))?;
    if !agent.is_awaiting_input() || agent.open_ask.is_none() {
        return Err(format!("{target} is not asking anything"));
    }
    Ok(agent)
}

fn parse_replies(
    args: &AnswerArgs,
    kind: AskKind,
    questions: &[AskQuestion],
) -> std::result::Result<Vec<AskReply>, String> {
    if let Some(file) = args.json.as_ref() {
        if !args.selectors.is_empty() || args.text.is_some() {
            return Err("--json cannot be combined with positional selectors or --text".to_owned());
        }
        let raw = match file {
            Some(path) => fs::read_to_string(path)
                .map_err(|err| format!("reading `{}`: {err}", path.display()))?,
            None => {
                let mut raw = String::new();
                std::io::stdin()
                    .read_to_string(&mut raw)
                    .map_err(|err| format!("reading JSON answers from stdin: {err}"))?;
                raw
            }
        };
        let values: Vec<JsonAnswer> =
            serde_json::from_str(&raw).map_err(|err| format!("invalid answer JSON: {err}"))?;
        return normalize_json_answers(&values, kind, questions);
    }

    if args.text.is_some() && questions.len() != 1 {
        return Err("--text is single-question-only; use --json for multiple questions".to_owned());
    }
    if args.text.is_some() && !args.selectors.is_empty() {
        return Err("mixing picks and text requires --json".to_owned());
    }
    if args.selectors.len() != questions.len()
        && !(questions.len() == 1 && args.selectors.is_empty() && args.text.is_some())
    {
        return Err(format!(
            "expected {} positional answer{}, got {}",
            questions.len(),
            if questions.len() == 1 { "" } else { "s" },
            args.selectors.len()
        ));
    }
    questions
        .iter()
        .enumerate()
        .map(|(index, question)| {
            let picks = args
                .selectors
                .get(index)
                .map(|raw| {
                    raw.split(',')
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(|value| resolve_answer_selector(kind, value, question))
                        .collect::<std::result::Result<Vec<_>, _>>()
                })
                .transpose()?
                .unwrap_or_default();
            validate_reply(
                kind,
                question,
                AskReply {
                    picks,
                    text: args.text.clone(),
                },
            )
        })
        .collect()
}

fn normalize_json_answers(
    values: &[JsonAnswer],
    kind: AskKind,
    questions: &[AskQuestion],
) -> std::result::Result<Vec<AskReply>, String> {
    if values.len() != questions.len() {
        return Err(format!(
            "expected {} JSON answer objects, got {}",
            questions.len(),
            values.len()
        ));
    }
    values
        .iter()
        .zip(questions)
        .map(|(value, question)| {
            let picks = value
                .pick
                .iter()
                .map(|pick| match pick {
                    JsonPick::Index(index) => resolve_answer_index(kind, *index, question),
                    JsonPick::Label(label) => resolve_answer_selector(kind, label, question),
                })
                .collect::<std::result::Result<Vec<_>, _>>()?;
            validate_reply(
                kind,
                question,
                AskReply {
                    picks,
                    text: value.text.clone(),
                },
            )
        })
        .collect()
}

fn validate_reply(
    kind: AskKind,
    question: &rimz::transcript::AskQuestion,
    reply: AskReply,
) -> std::result::Result<AskReply, String> {
    if reply.text.is_some()
        && let Some(message) = menu_action_error(kind, question)
    {
        return Err(message);
    }
    if reply
        .text
        .as_deref()
        .is_some_and(|text| text.trim().is_empty())
    {
        return Err("answer text cannot be empty".to_owned());
    }
    if reply.picks.is_empty() && reply.text.as_deref().is_none_or(str::is_empty) {
        return Err(format!(
            "answer is empty; valid options: {}",
            valid_options(question)
        ));
    }
    if reply.picks.len() > 1 && !question.multi_select {
        return Err(format!(
            "question is single-select; valid options: {}",
            valid_options(question)
        ));
    }
    let mut unique = reply.picks.clone();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() != reply.picks.len() {
        return Err("an option can be selected only once".to_owned());
    }
    Ok(reply)
}

fn resolve_answer_selector(
    kind: AskKind,
    selector: &str,
    question: &rimz::transcript::AskQuestion,
) -> std::result::Result<usize, String> {
    resolve_selector(selector, &question.options)
        .map_err(|error| menu_action_error(kind, question).unwrap_or(error))
}

fn resolve_answer_index(
    kind: AskKind,
    index: usize,
    question: &rimz::transcript::AskQuestion,
) -> std::result::Result<usize, String> {
    resolve_index(index, &question.options)
        .map_err(|error| menu_action_error(kind, question).unwrap_or(error))
}

fn menu_action_error(kind: AskKind, question: &rimz::transcript::AskQuestion) -> Option<String> {
    let valid = valid_options(question);
    match kind {
        AskKind::Permission => Some(format!(
            "permission asks accept only the listed remote option; use the agent pane for deny or any other action; valid options: {valid}"
        )),
        AskKind::PlanApproval => Some(format!(
            "plan approvals accept only the listed remote option; use the agent pane for keep-planning, refinement text, or manual-review approval; valid options: {valid}"
        )),
        AskKind::Question => None,
    }
}

fn resolve_selector(
    selector: &str,
    options: &[rimz::transcript::AskOption],
) -> std::result::Result<usize, String> {
    if let Ok(index) = selector.parse::<usize>() {
        return resolve_index(index, options);
    }
    let matches = options
        .iter()
        .enumerate()
        .filter(|(_, option)| option.label.eq_ignore_ascii_case(selector))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [index] => Ok(*index),
        [] => Err(format!(
            "unknown option `{selector}`; valid options: {}",
            valid_options_slice(options)
        )),
        _ => Err(format!("option label `{selector}` is ambiguous")),
    }
}

fn resolve_index(
    index: usize,
    options: &[rimz::transcript::AskOption],
) -> std::result::Result<usize, String> {
    if index == 0 || index > options.len() {
        return Err(format!(
            "option {index} is out of range; valid options: {}",
            valid_options_slice(options)
        ));
    }
    Ok(index - 1)
}

fn valid_options(question: &rimz::transcript::AskQuestion) -> String {
    valid_options_slice(&question.options)
}

fn valid_options_slice(options: &[rimz::transcript::AskOption]) -> String {
    options
        .iter()
        .enumerate()
        .map(|(index, option)| format!("{}={}", index + 1, option.label))
        .collect::<Vec<_>>()
        .join(", ")
}

fn wait_for_confirmation(
    store: &rimz::Store,
    kind: &rimz::ids::AgentKind,
    agent_id: &rimz::ids::AgentSessionId,
    ask_id: &AskId,
    timeout: Duration,
) -> Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        let snapshot = store
            .snapshot_cached()
            .context("checking answer confirmation")?;
        let still_open = snapshot.agents.iter().any(|agent| {
            &agent.kind == kind
                && &agent.agent_id == agent_id
                && agent.is_awaiting_input()
                && agent.open_ask.as_ref().is_some_and(|ask| &ask.id == ask_id)
        });
        if !still_open || transcript_has_answer(store.paths(), ask_id)? {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        sleep(CONFIRM_POLL);
    }
}

fn record_answer_if_missing(
    store: &rimz::Store,
    agent: &rimz::agents::AgentState,
    ask_id: &AskId,
    questions: &[AskQuestion],
    replies: &[AskReply],
) -> Result<()> {
    if transcript_has_answer(store.paths(), ask_id)? {
        return Ok(());
    }
    let answers = questions
        .iter()
        .zip(replies)
        .map(|(question, reply)| {
            let mut chosen = reply
                .picks
                .iter()
                .filter_map(|index| question.options.get(*index))
                .map(|option| option.label.clone())
                .collect::<Vec<_>>();
            if let Some(text) = reply.text.clone() {
                chosen.push(text);
            }
            AskAnswer {
                question: Some(question.question.clone()),
                chosen,
                note: None,
            }
        })
        .collect::<Vec<_>>();
    let mut entry = TranscriptEntry::new(
        jiff::Timestamp::now(),
        agent.kind.clone(),
        agent.agent_id.clone(),
        TranscriptKind::Answer,
        rimz::transcript::answers_text(&answers),
    );
    entry.id = Some(ask_id.clone());
    entry.channel =
        rimz::transcript::entry_channel(agent.channel.as_deref(), agent.worktree_path.as_deref());
    entry.name = agent.name.clone();
    entry.profile = agent.profile.clone();
    entry.role = agent.role.clone();
    entry.from = Some("you".to_owned());
    entry.answers = answers;
    rimz::transcript::append_answer_if_missing(store.paths(), &entry)?;
    Ok(())
}

fn transcript_has_answer(paths: &rimz::StatePaths, ask_id: &AskId) -> Result<bool> {
    Ok(rimz::transcript::read_all(paths)?
        .into_iter()
        .any(|entry| entry.entry == TranscriptKind::Answer && entry.id.as_ref() == Some(ask_id)))
}

fn parse_wait(raw: &str) -> std::result::Result<Duration, String> {
    rimz::harness::schedule::parse_duration_units(raw, &[("s", 1), ("m", 60), ("h", 3600)])
}

fn answer_exit(code: i32, message: &str) -> ! {
    let mut err = super::render::err();
    let _ = writeln!(err, "error: {message}");
    std::process::exit(code);
}

#[cfg(test)]
mod tests;
