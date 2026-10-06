//! Self-explaining wait headlines, evidence, and verbatim notes.

use jiff::Timestamp;
use serde_json::{Map, Value};

use crate::config::{TaskEntry, WaitMeta, WatchSpec};
use crate::harness::schedule::signal::{Signal, elapsed_label, match_value};

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
/// the launch reminder's Loop section already carries the standing frame.
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
    format!(
        "Rule `{name}` fired here. {}\n\n{prompt}",
        event.join(" · ")
    )
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
    let payload = &signal.payload;
    let text = |key: &str| payload.get(key).and_then(Value::as_str);
    let mut headline = signal.name.to_string();
    match signal.name.family() {
        family @ ("ci" | "pr") => {
            if let Some(branch) = text("branch") {
                headline.push_str(&format!(" on {}", one_line(branch)));
            }
            if let Some(number) = payload.get("number").and_then(Value::as_u64) {
                headline.push_str(&format!(" #{number}"));
            }
            if family == "ci"
                && let Some(head) = text("head")
            {
                headline.push_str(&format!(" @{}", one_line(short_sha(head))));
            }
        }
        "agent" => {
            let key = if text("handle").is_some() {
                "handle"
            } else {
                "session"
            };
            append_identity(&mut headline, payload, key);
        }
        "trunk" => {
            if let Some(trunk) = text("trunk") {
                headline.push_str(&format!(" on {}", one_line(trunk)));
            }
        }
        "worktree" => append_identity(&mut headline, payload, "name"),
        "team" => append_identity(&mut headline, payload, "instance"),
        _ => {}
    }
    match signal.name.as_str() {
        "pr.behind" => {
            if let Some(behind) = payload.get("behind_by").and_then(Value::as_u64) {
                headline.push_str(&format!(" · {behind} behind"));
                append_identity(&mut headline, payload, "base");
            }
        }
        "pr.conflicted" => {
            if let Some(base) = text("base") {
                headline.push_str(&format!(" · with {}", one_line(base)));
            }
        }
        "pr.dequeued" => {
            if let Some(reason) = text("reason") {
                headline.push_str(&format!(" · {}", one_line(reason)));
            }
        }
        "trunk.moved" => {
            if let (Some(from), Some(to)) = (text("from"), text("to")) {
                let (from, to) = (one_line(short_sha(from)), one_line(short_sha(to)));
                headline.push_str(&format!(" {from}..{to}"));
            }
        }
        "worktree.created" => {
            if let Some(base) = text("base") {
                headline.push_str(&format!(" from {}", one_line(base)));
            }
        }
        "worktree.removed" => {
            if payload.get("branch_deleted").and_then(Value::as_bool) == Some(true) {
                headline.push_str(" · branch deleted");
            }
        }
        "team.failed" => {
            if let Some(member) = text("member") {
                let handle = member.split('#').next().unwrap_or(member);
                headline.push_str(&format!(" · {}", one_line(handle)));
            }
        }
        "team.stage" => {
            if let Some(to) = text("to").map(one_line) {
                match text("from").map(one_line) {
                    Some(from) => headline.push_str(&format!(" · {from} -> {to}")),
                    None => headline.push_str(&format!(" · {to}")),
                }
            }
        }
        _ => {}
    }
    headline
}

/// The lines under the verdict: nothing for a built-in family, whose agent
/// can ask the CLI, and every top-level field for a custom one.
fn signal_details(signal: &Signal) -> Vec<String> {
    if !signal.name.is_reserved() {
        return signal
            .payload
            .iter()
            .map(|(key, value)| format!("{}: {}", one_line(key), one_line(&match_value(value))))
            .collect();
    }
    if signal.name.as_str() != "pr.dequeued" {
        return Vec::new();
    }
    // Nothing else names the queue commit, and `#N` cannot find the failed run.
    signal
        .payload
        .get("queue_checks_url")
        .and_then(Value::as_str)
        .map(|url| format!("queue checks: {}", one_line(url)))
        .into_iter()
        .collect()
}

/// A key, value, or subject segment bare, or JSON-quoted when it holds a line
/// break, so it stays on its one line.
fn one_line(text: &str) -> String {
    if text.contains(['\n', '\r']) {
        return Value::from(text).to_string();
    }
    text.to_owned()
}

fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

fn append_identity(headline: &mut String, payload: &Map<String, Value>, key: &str) {
    if let Some(identity) = payload.get(key).and_then(Value::as_str) {
        headline.push(' ');
        headline.push_str(&one_line(identity));
    }
}

fn subscription_scope(task: &TaskEntry) -> String {
    let Some(matches) = &task.matches else {
        return String::new();
    };
    for key in ["branch", "path", "instance", "team", "handle", "session"] {
        if let Some(scope) = matches.get(key)
            && !crate::harness::schedule::signal::is_match_wildcard(scope)
        {
            return format!(" on {scope}");
        }
    }
    String::new()
}

#[cfg(test)]
mod tests;
