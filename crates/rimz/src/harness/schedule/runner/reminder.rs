//! The `### Loop` launch-reminder body: what launched the agent, how long it
//! lives, and who is watching, composed in the firing process.

use std::time::Duration;

use crate::config::{CheckOn, TaskEntry};
use crate::harness::run::VERIFY_MAX_ATTEMPTS_DEFAULT;
use crate::harness::schedule::arm::{default_signal_match_key, duration_label};
use crate::harness::schedule::catalog::LoadedTask;
use crate::harness::schedule::run_log::LoopRunMode;
use crate::harness::schedule::signal::SignalSelector;
use crate::harness::schedule::{ParsedSchedule, ParsedTrigger, Schedule, Trigger};

/// One launching fire, as the section describes it.
pub(super) struct LoopFire<'a> {
    pub name: &'a str,
    pub task: &'a LoadedTask,
    pub mode: LoopRunMode,
    pub keep: bool,
    /// The single run's resolved timeout; a resident has none.
    pub timeout: Option<Duration>,
}

/// The section body, without heading or tag.
pub(super) fn compose(fire: &LoopFire<'_>) -> String {
    let entry = fire.task.entry();
    let mut standing = Vec::new();
    if entry.stay {
        standing.push("You run once here and stay on afterwards.".to_owned());
    } else {
        if !fire.task.is_ephemeral() {
            standing.push("Each run is a fresh agent with no memory of earlier runs.".to_owned());
        }
        standing.extend(turn_sentence(entry.verify.is_some(), fire.keep).map(str::to_owned));
    }
    standing.push(
        match (fire.mode, entry.stay) {
            (LoopRunMode::Manual, _) => "The user is watching this run.",
            (LoopRunMode::Scheduled, true) => {
                "Nobody is watching this turn. A question for the user goes in your final message, and they may reply here later."
            }
            (LoopRunMode::Scheduled, false) => {
                "Nobody is watching. Your final message is the result. Ask only when you cannot go on: a question waits until the user notices or the run is stopped."
            }
        }
        .to_owned(),
    );
    if entry.stay {
        standing.extend(subscriptions(entry));
    } else {
        if let Some(cmd) = &entry.verify {
            standing.push(format!(
                "When your turn ends, {} runs. If it fails, you get its output and another turn, up to {} turns.",
                span(cmd),
                entry.max_attempts.unwrap_or(VERIFY_MAX_ATTEMPTS_DEFAULT)
            ));
        }
        if let Some(timeout) = fire.timeout {
            standing.push(format!(
                "The run is stopped after {}.",
                duration_label(timeout)
            ));
        }
    }
    let standing = standing.join(" ");
    match fire.mode {
        LoopRunMode::Manual => format!(
            "The user fired the rule {} by hand. {standing}",
            span(fire.name)
        ),
        LoopRunMode::Scheduled => format!("{}\n\n{standing}", origin(fire.name, fire.task)),
    }
}

fn turn_sentence(verify: bool, keep: bool) -> Option<&'static str> {
    match (verify, keep) {
        (false, false) => Some("This is one turn, and the pane closes when it ends."),
        (true, false) => Some("The pane closes when the run ends."),
        (false, true) => Some("This is one turn."),
        (true, true) => None,
    }
}

fn origin(name: &str, task: &LoadedTask) -> String {
    let entry = task.entry();
    let subject = if entry.each_worktree {
        "one agent in each worktree"
    } else {
        "an agent"
    };
    let trigger = task.trigger().as_ref().ok();
    let condition = matches!(
        trigger,
        Some(ParsedTrigger {
            trigger: Trigger::Condition { .. },
            ..
        })
    );
    let check = check_clause(entry, condition);
    let mut origin = format!(
        "RimZ started you from the rule {}, which launches {subject}{}{}.",
        span(name),
        trigger.map_or_else(String::new, |trigger| trigger_clause(trigger, entry)),
        check.as_deref().unwrap_or_default()
    );
    if check.is_some() {
        origin.push_str(" The check's output follows the prompt.");
    }
    origin.push_str(" The prompt is the rule's fixed text, not a message someone just typed.");
    origin
}

fn trigger_clause(parsed: &ParsedTrigger, entry: &TaskEntry) -> String {
    match &parsed.trigger {
        // `describe` prints a raw cron as a noun phrase, which no verb can follow.
        Trigger::Schedule(ParsedSchedule {
            schedule: Schedule::RawCron(expr),
            ..
        }) => format!(" on the cron schedule {}", span(expr)),
        Trigger::Schedule(_) => format!(" {}", verbatim(&parsed.describe())),
        Trigger::Signal { .. } => {
            let described = parsed.describe();
            let selector = described.strip_prefix("on ").unwrap_or(&described);
            format!(" on the signal {}", span(selector))
        }
        Trigger::Condition { expr, hold } => {
            let mut clause = format!(" when {} holds", span(&expr.to_string()));
            if entry.each_worktree {
                clause.push_str(" there");
            }
            if let Some(hold) = hold {
                clause.push_str(&format!(" for {}", duration_label(*hold)));
            }
            clause
        }
        Trigger::Watch(_) => String::new(),
    }
}

/// A resident runs no check: the ladder never reads one.
fn check_clause(entry: &TaskEntry, after_condition: bool) -> Option<String> {
    let crate::config::TaskCheck::Shell(cmd) = entry.check.as_ref().filter(|_| !entry.stay)? else {
        return None;
    };
    let joiner = if after_condition { "and" } else { "when" };
    Some(match entry.on.unwrap_or_default() {
        CheckOn::Fail => format!(" {joiner} its check {} fails", span(cmd)),
        CheckOn::Success => format!(" {joiner} its check {} passes", span(cmd)),
        CheckOn::Any => format!(" after its check {} runs", span(cmd)),
    })
}

fn subscriptions(entry: &TaskEntry) -> Option<String> {
    let names = entry
        .subscribe
        .iter()
        .map(|binding| span(&binding.signal))
        .collect::<Vec<_>>();
    let list = match names.as_slice() {
        [] => return None,
        [one] => one.clone(),
        [first, second] => format!("{first} and {second}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    };
    let worktree = entry.subscribe.iter().all(|binding| {
        binding
            .signal
            .parse::<SignalSelector>()
            .is_ok_and(|selector| {
                default_signal_match_key(&selector, &binding.matches) == Some("path")
            })
    });
    Some(format!(
        "While you stay, {list}{} {} you as messages.",
        if worktree { " for this worktree" } else { "" },
        if names.len() == 1 { "reaches" } else { "reach" }
    ))
}

fn span(text: &str) -> String {
    format!("`{}`", verbatim(text))
}

/// User text shown as typed: only `<`, which could open or close the
/// reminder tag, and control characters are escaped.
fn verbatim(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '<' => escaped.push_str("&lt;"),
            ch if ch.is_control() => escaped.extend(ch.escape_default()),
            ch => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests;
