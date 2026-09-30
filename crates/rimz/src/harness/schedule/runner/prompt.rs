//! Self-explaining wait headlines, evidence, and verbatim notes.

use jiff::Timestamp;
use serde_json::{Map, Value};

use crate::config::{TaskEntry, WaitMeta, WatchSpec};
use crate::harness::schedule::signal::{Signal, elapsed_label};

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
        body.push('\n');
        let mut payload = signal.payload.clone();
        payload.insert("signal".to_owned(), Value::String(signal.name.to_string()));
        body.push_str(&Value::Object(payload).to_string());
    }
    if checkin {
        body.push_str(&format!("\n\nStop it: rimz wait cancel {name}"));
        if let Some(delay) = &task.timeout {
            body.push_str(&format!("\nAnother check-in: rimz wait --in {delay}"));
        }
    }
    if let Evidence::Condition(condition) = &evidence {
        body.push('\n');
        // String keys and optional string readings are always JSON-serializable.
        body.push_str(
            &serde_json::to_string(&condition.readings)
                .expect("condition readings are JSON strings"),
        );
    }
    if !note.is_empty() {
        body.push_str("\n\n");
        body.push_str(note);
    }
    body
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
    let mut headline = signal.name.to_string();
    match signal.name.family() {
        "ci" | "pr" => {
            if let Some(branch) = signal.payload.get("branch").and_then(Value::as_str) {
                headline.push_str(&format!(" on {branch}"));
            }
            if let Some(number) = signal.payload.get("number").and_then(Value::as_u64) {
                headline.push_str(&format!(" (PR #{number})"));
            }
        }
        "agent" => append_identity(&mut headline, &signal.payload, "handle"),
        "trunk" => {
            if let Some(trunk) = signal.payload.get("trunk").and_then(Value::as_str) {
                headline.push_str(&format!(" on {trunk}"));
            }
        }
        "worktree" => append_identity(&mut headline, &signal.payload, "branch"),
        "team" => append_identity(&mut headline, &signal.payload, "instance"),
        _ => {}
    }
    headline
}

fn append_identity(headline: &mut String, payload: &Map<String, Value>, key: &str) {
    if let Some(identity) = payload.get(key).and_then(Value::as_str) {
        headline.push(' ');
        headline.push_str(identity);
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
