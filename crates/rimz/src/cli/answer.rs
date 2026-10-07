//! `rimz answer` — validate and drive one current native prompt atomically.

use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Args;
use serde::Deserialize;

use super::{Ctx, GlobalFlags, ask_commands, ask_pane_owner, resolve_open_ask};
use rimz::agents::{AnswerStep, AskReply};
use rimz::ids::AskId;
use rimz::mux::PaneWriter;
use rimz::transcript::{AskAnswer, AskQuestion, TranscriptEntry, TranscriptKind};
use rimz::utils::time::{DurationUnit, parse_duration_units};

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
    let peers = rimz::address::addressable_agents(&snapshot);
    let agent = resolve_current_agent(store, &snapshot, &args.target, &ctx.address_context())
        .unwrap_or_else(|error| {
            super::render::report(&error);
            std::process::exit(2);
        });
    let target_id = args
        .target
        .starts_with("ask_")
        .then(|| AskId::parse(&args.target))
        .transpose()?;
    let detail = rimz::agents::read_open_ask(store.paths(), agent, target_id.as_ref())
        .unwrap_or_else(|err| answer_exit(2, &err.to_string()))
        .unwrap_or_else(|| {
            answer_exit(
                2,
                &format!(
                    "{} is not asking anything",
                    rimz::address::agent_handle(agent, &peers, true)
                ),
            )
        });
    let ask_id = detail.open.id.clone();
    let kind = agent.kind.clone();
    let agent_id = agent.agent_id.clone();
    let (owner, child_name) = ask_pane_owner(agent, &snapshot.agents)
        .unwrap_or_else(|| answer_exit(2, &format!("ask `{ask_id}` has no live bound pane")));
    let handle = rimz::address::agent_handle(owner, &peers, true);
    let who = child_name
        .as_ref()
        .map(|name| format!("{name} (via {handle})"))
        .unwrap_or_else(|| handle.clone());
    let commands = ask_commands::AskCommands {
        detail: &detail,
        pane: &handle,
        target: if child_name.is_some() {
            ask_id.as_str()
        } else {
            &handle
        },
    };
    if let Some(message) = commands.pane_refusal(&who) {
        answer_exit(3, &message);
    }
    let adapter = rimz::agents::definition_by_kind(kind.as_str())
        .unwrap_or_else(|err| answer_exit(3, &err.to_string()));
    if args.selectors.is_empty() && args.text.is_none() && args.json.is_none() {
        answer_exit(3, &commands.missing_answer(&who));
    }
    let replies =
        parse_replies(&args, &commands).unwrap_or_else(|message| answer_exit(3, &message));
    let steps = adapter
        .answer_plan(detail.open.kind, &detail.questions, &replies)
        .unwrap_or_else(|err| answer_exit(3, &format!("{err}; in the pane: {}", commands.focus())));

    let live = ctx.resolution_snapshot()?;
    let target = live
        .live_agent_pane(&owner.kind, &owner.agent_id)
        .unwrap_or_else(|| answer_exit(2, &format!("{handle} has no live bound pane")));
    let writer = PaneWriter::open(store.runtime_paths(), &target, &ctx.workspace.session_name)
        .unwrap_or_else(|err| answer_exit(2, &format!("sending answer to {handle}: {err}")));

    // Re-read immediately before the first keystroke. This is the compare half
    // of the ask-id CAS; a prompt answered or superseded during validation gets
    // no input from this command.
    let current = store.snapshot_cached().context("rechecking current ask")?;
    let still_current = current.agents.iter().any(|agent| {
        agent.kind == kind
            && agent.agent_id == agent_id
            && agent.actionable_asks().any(|(ask, _)| ask.id == ask_id)
    });
    if !still_current {
        answer_exit(
            2,
            &format!("ask `{ask_id}` was answered or replaced before any key was sent"),
        );
    }

    let mut pacer = rimz::message::send::Pacer::from_env();
    for step in steps {
        pacer.tick();
        let result = match step {
            AnswerStep::Text(text) => writer.type_text(&text),
            AnswerStep::Key(key) => writer.press(key),
            AnswerStep::Paste(text) => writer.paste(&text),
        };
        if let Err(err) = result {
            answer_exit(2, &format!("sending answer to {handle}: {err}"));
        }
    }
    drop(writer);

    if args.no_wait {
        let mut out = super::render::out();
        writeln!(
            out,
            "sent answer to {who}: {}  {ask_id}",
            ask_commands::choice(&detail, &replies)
        )?;
        return Ok(());
    }

    let wait = args.wait.unwrap_or(DEFAULT_ANSWER_WAIT);
    if !wait_for_confirmation(store, &kind, &agent_id, &ask_id, wait)? {
        answer_exit(
            4,
            &format!(
                "typed the answer into {handle}'s pane, but it did not confirm within {wait:?}\n  do not resend. check: rimz asks show {}   or look: rimz pane capture {handle}",
                commands.target
            ),
        );
    }
    record_answer_if_missing(store, agent, &ask_id, &detail.questions, &replies)?;
    let mut out = super::render::out();
    writeln!(
        out,
        "answered {who}: {}  {ask_id}",
        ask_commands::choice(&detail, &replies)
    )?;
    Ok(())
}

fn resolve_current_agent<'a>(
    store: &rimz::Store,
    snapshot: &'a rimz::store::snapshot::SidebarSnapshot,
    target: &str,
    channel: &rimz::address::AddressContext,
) -> Result<&'a rimz::agents::AgentState> {
    let agent = resolve_open_ask(store, snapshot, target, channel)?
        .ok_or_else(|| anyhow::anyhow!(ask_commands::unknown_ask(target)))?;
    if agent.actionable_asks().next().is_none() {
        let peers = rimz::address::addressable_agents(snapshot);
        return Err(anyhow::anyhow!(
            "{} is not asking anything",
            rimz::address::agent_handle(agent, &peers, true)
        ));
    }
    Ok(agent)
}

fn parse_replies(
    args: &AnswerArgs,
    commands: &ask_commands::AskCommands<'_>,
) -> std::result::Result<Vec<AskReply>, String> {
    let questions = &commands.detail.questions;
    let menu_refusal = commands.menu_refusal();
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
        return normalize_json_answers(&values, questions, menu_refusal.as_deref());
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
                        .map(|value| {
                            resolve_selector(value, &question.options)
                                .map_err(|message| menu_refusal.clone().unwrap_or(message))
                        })
                        .collect::<std::result::Result<Vec<_>, _>>()
                })
                .transpose()?
                .unwrap_or_default();
            validate_reply(
                question,
                AskReply {
                    picks,
                    text: args.text.clone(),
                },
                menu_refusal.as_deref(),
            )
        })
        .collect()
}

fn normalize_json_answers(
    values: &[JsonAnswer],
    questions: &[AskQuestion],
    menu_refusal: Option<&str>,
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
                .map(|pick| {
                    match pick {
                        JsonPick::Index(index) => resolve_index(*index, &question.options),
                        JsonPick::Label(label) => resolve_selector(label, &question.options),
                    }
                    .map_err(|message| menu_refusal.map_or(message, str::to_owned))
                })
                .collect::<std::result::Result<Vec<_>, _>>()?;
            validate_reply(
                question,
                AskReply {
                    picks,
                    text: value.text.clone(),
                },
                menu_refusal,
            )
        })
        .collect()
}

fn validate_reply(
    question: &rimz::transcript::AskQuestion,
    reply: AskReply,
    menu_refusal: Option<&str>,
) -> std::result::Result<AskReply, String> {
    if reply.text.is_some()
        && let Some(message) = menu_refusal
    {
        return Err(message.to_owned());
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
    let mut unique = reply.picks.clone();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() != reply.picks.len() {
        return Err("an option can be selected only once".to_owned());
    }
    if reply.picks.len() > 1 && !question.multi_select {
        return Err(format!(
            "question is single-select; valid options: {}",
            valid_options(question)
        ));
    }
    Ok(reply)
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
                && agent.actionable_asks().any(|(ask, _)| &ask.id == ask_id)
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
    entry.from = Some(rimz::transcript::HUMAN_FROM.to_owned());
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
    parse_duration_units(
        raw,
        &[
            DurationUnit::Second,
            DurationUnit::Minute,
            DurationUnit::Hour,
        ],
    )
    .map_err(|err| err.to_string())
}

fn answer_exit(code: i32, message: &str) -> ! {
    let mut err = super::render::err();
    let _ = writeln!(err, "error: {message}");
    std::process::exit(code);
}

#[cfg(test)]
mod tests;
