//! Self-explaining wake headlines, evidence, and verbatim notes.

use jiff::Timestamp;
use serde_json::{Map, Value};

use crate::config::{TaskEntry, WakeMeta};
use crate::harness::schedule::signal::{Signal, elapsed_label};

pub(super) enum Evidence<'a> {
    Scheduled,
    Signal(&'a Signal),
    Manual,
}

pub(super) fn compose_wake(
    name: &str,
    task: &TaskEntry,
    meta: Option<&WakeMeta>,
    evidence: Evidence<'_>,
    note: &str,
    now: Timestamp,
) -> String {
    let mut body = String::new();
    body.push_str(&wait_line(task, meta, &evidence));
    if let Some(verdict) = verdict_line(&evidence, meta, now, name) {
        body.push('\n');
        body.push_str(&verdict);
    } else {
        body.push_str(&format!(" [{name}]"));
    }
    if let Evidence::Signal(signal) = &evidence {
        body.push('\n');
        match &signal.watch {
            Some(watch) if watch.output.is_empty() => body.push_str("(no output)"),
            Some(watch) => body.push_str(&watch.output),
            None => {
                let mut payload = signal.payload.clone();
                payload.insert("signal".to_owned(), Value::String(signal.name.to_string()));
                body.push_str(&Value::Object(payload).to_string());
            }
        }
    }
    if let Evidence::Signal(signal) = &evidence
        && signal
            .watch
            .as_ref()
            .is_some_and(|watch| !watch.verdict.is_terminal())
    {
        let delay = task.timeout.as_deref().unwrap_or("30m");
        body.push_str(&format!(
            "\n\nStop it: rimz wake cancel {name}\nAnother check-in: rimz wake --in {delay}"
        ));
    }
    if !note.is_empty() {
        body.push_str("\n\n");
        body.push_str(note);
    }
    body
}

fn wait_line(task: &TaskEntry, meta: Option<&WakeMeta>, evidence: &Evidence<'_>) -> String {
    if let Some(command) = &task.watch {
        return format!(
            "waited on `{}`",
            crate::theme::fmt::command_preview(command)
        );
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
    "scheduled wake".to_owned()
}

fn verdict_line(
    evidence: &Evidence<'_>,
    meta: Option<&WakeMeta>,
    now: Timestamp,
    name: &str,
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
        Evidence::Scheduled if meta.is_some_and(|meta| meta.delay.is_some()) => return None,
        Evidence::Scheduled => "fired".to_owned(),
    };
    if let Evidence::Signal(signal) = evidence
        && let Some(path) = signal
            .watch
            .as_ref()
            .filter(|watch| !watch.output.is_empty())
            .and_then(|watch| watch.output_path.as_ref())
    {
        verdict.push_str(&format!(" · output: {}", path.display()));
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
        if let Some(scope) = matches.get(key) {
            return format!(" on {scope}");
        }
    }
    String::new()
}

#[cfg(test)]
mod tests;
