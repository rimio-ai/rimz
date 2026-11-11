//! Self-explaining wait headlines, evidence, and verbatim notes.

use jiff::Timestamp;

use crate::config::{TaskEntry, WaitMeta, WatchSpec};
use crate::harness::signal::{Signal, elapsed_label, one_line, signal_details, signal_subject};

pub(super) enum Evidence<'a> {
    Scheduled,
    Signal(&'a Signal),
    Condition(&'a super::super::when::ConditionEvidence),
    Manual,
}

pub(super) fn compose_wait(
    name: &str,
    task: &TaskEntry,
    meta: Option<&WaitMeta>,
    evidence: Evidence<'_>,
    note: &str,
    now: Timestamp,
) -> String {
    let checkin = match &evidence {
        Evidence::Signal(signal) => signal
            .watch
            .as_ref()
            .is_some_and(|watch| !watch.verdict.is_terminal()),
        _ => false,
    };
    let continuation = if checkin { " · still watching" } else { "" };
    let mut body = String::new();
    body.push_str(&wait_line(task, meta, &evidence));
    if let Some(verdict) = verdict_line(task, &evidence, meta, now, name, continuation) {
        body.push('\n');
        body.push_str(&verdict);
    } else {
        body.push_str(&format!(" [{name}]"));
    }
    if let Evidence::Signal(signal) = &evidence
        && signal.watch.is_none()
    {
        for line in signal_details(signal) {
            body.push('\n');
            body.push_str(&line);
        }
    }
    if checkin {
        body.push_str(&format!("\n\nStop it: rimz wait cancel {name}"));
        if let Some(delay) = &task.timeout {
            body.push_str(&format!("\nAnother check-in: rimz wait --in {delay}"));
        }
    }
    if let Evidence::Condition(condition) = &evidence {
        for (key, reading) in &condition.readings {
            let reading = reading
                .as_deref()
                .map_or_else(|| "unknown".to_owned(), one_line);
            body.push_str(&format!("\n{}: {reading}", one_line(key)));
        }
    }
    if !note.is_empty() {
        body.push_str("\n\n");
        body.push_str(note);
    }
    body
}

/// The message a launching fire opens its agent with: the event that fired
/// the rule, then the prompt. A fire with no event is the prompt alone, since
/// the launch reminder's Loop section already carries the standing frame, and
/// an empty prompt leaves the event alone.
pub(super) fn compose_launch(name: &str, evidence: &Evidence<'_>, prompt: &str) -> String {
    let event = match evidence {
        Evidence::Condition(condition) => condition
            .readings
            .iter()
            .map(|(key, reading)| {
                let reading = reading
                    .as_deref()
                    .map_or_else(|| "unknown".to_owned(), one_line);
                format!("{}: {reading}", one_line(key))
            })
            .collect::<Vec<_>>(),
        Evidence::Signal(signal) => {
            let mut event = vec![signal_headline(signal)];
            event.extend(signal_details(signal));
            event
        }
        Evidence::Scheduled | Evidence::Manual => return prompt.to_owned(),
    };
    let event = format!("Rule `{name}` fired here. {}", event.join(" · "));
    if prompt.is_empty() {
        return event;
    }
    format!("{event}\n\n{prompt}")
}

fn wait_line(task: &TaskEntry, meta: Option<&WaitMeta>, evidence: &Evidence<'_>) -> String {
    if let Evidence::Condition(condition) = evidence {
        return format!("waited on {}", condition.when);
    }
    if let Some(clauses) = &task.when
        && let Ok(expr) = super::super::when::WhenExpr::parse(clauses)
    {
        return format!("waited on {expr}");
    }
    if let Some(spec) = &task.watch {
        return spec.headline();
    }
    if let Evidence::Signal(signal) = evidence {
        return format!("waited on {}", signal_headline(signal));
    }
    if let Some(selector) = &task.signal {
        return format!("waited on {selector}{}", subscription_scope(task));
    }
    if let Some(delay) = meta.and_then(|meta| meta.delay.as_deref()) {
        return format!("waited {delay}");
    }
    "scheduled wait".to_owned()
}

fn verdict_line(
    task: &TaskEntry,
    evidence: &Evidence<'_>,
    meta: Option<&WaitMeta>,
    now: Timestamp,
    name: &str,
    continuation: &str,
) -> Option<String> {
    let mut verdict = match evidence {
        Evidence::Signal(signal) => match &signal.watch {
            Some(watch) => watch.verdict.label(),
            None => match meta {
                Some(meta) => {
                    let elapsed_ms = now
                        .as_millisecond()
                        .saturating_sub(meta.armed_at.as_millisecond())
                        .max(0) as u64;
                    format!("fired after {}", elapsed_label(elapsed_ms))
                }
                None => "fired".to_owned(),
            },
        },
        Evidence::Manual => "fired by hand".to_owned(),
        Evidence::Condition(condition) => condition
            .hold
            .as_ref()
            .map_or_else(|| "fired".to_owned(), |hold| format!("held {hold}")),
        Evidence::Scheduled if meta.is_some_and(|meta| meta.delay.is_some()) => return None,
        Evidence::Scheduled => "fired".to_owned(),
    };
    verdict.push_str(continuation);
    if let Evidence::Signal(signal) = evidence
        && let Some(watch) = &signal.watch
        && let Some(path) = &watch.output_path
    {
        if !watch.summary.is_empty() {
            verdict.push_str(&format!(
                " · output: {} ({})",
                path.display(),
                watch.summary.label()
            ));
        } else if matches!(task.watch, Some(WatchSpec::Command(_))) {
            // Only a watched command's output is the thing waited on; a polled
            // watch's empty file says nothing about its condition.
            verdict.push_str(" · no output");
        }
    }
    verdict.push_str(&format!(" [{name}]"));
    Some(verdict)
}

fn signal_headline(signal: &Signal) -> String {
    let subject = signal_subject(&signal.name, &signal.payload);
    if subject.is_empty() {
        signal.name.to_string()
    } else {
        format!("{} {subject}", signal.name)
    }
}

fn subscription_scope(task: &TaskEntry) -> String {
    let Some(matches) = &task.matches else {
        return String::new();
    };
    for key in ["branch", "path", "instance", "team", "handle", "session"] {
        if let Some(scope) = matches.get(key)
            && !crate::harness::signal::is_match_wildcard(scope)
        {
            return format!(" on {scope}");
        }
    }
    String::new()
}

#[cfg(test)]
mod tests;
