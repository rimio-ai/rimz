//! Self-explaining wake headlines, evidence, and verbatim notes.

use jiff::tz::TimeZone;
use serde_json::{Map, Value};

use crate::config::{TaskEntry, WakeArmer, WakeMeta};
use crate::harness::schedule::signal::{Signal, WatchOutcome};

pub(super) enum Evidence<'a> {
    Scheduled,
    Signal(&'a Signal),
    Manual,
    Expired,
}

pub(super) fn compose_wake(
    name: &str,
    task: &TaskEntry,
    meta: Option<&WakeMeta>,
    evidence: Evidence<'_>,
    note: &str,
    tz: TimeZone,
) -> String {
    let trigger = match evidence {
        Evidence::Expired => format!(
            "no {}{} in {}",
            task.signal.as_deref().unwrap_or_default(),
            subscription_scope(task),
            task.timeout.as_deref().unwrap_or_default()
        ),
        Evidence::Manual => "manual fire".to_owned(),
        Evidence::Scheduled => meta
            .and_then(|meta| meta.delay.as_deref())
            .map(|delay| format!("{delay} elapsed"))
            .unwrap_or_else(|| "scheduled wake".to_owned()),
        Evidence::Signal(signal) => match &signal.watch {
            Some(WatchOutcome::Exited {
                code, elapsed_ms, ..
            }) => format!(
                "`{}` exited {} after {}",
                task.watch.as_deref().unwrap_or_default(),
                code.map(|code| code.to_string())
                    .unwrap_or_else(|| "by signal".to_owned()),
                elapsed_label(*elapsed_ms)
            ),
            Some(WatchOutcome::TimedOut { elapsed_ms, .. }) => format!(
                "`{}` timed out after {}",
                task.watch.as_deref().unwrap_or_default(),
                elapsed_label(*elapsed_ms)
            ),
            Some(WatchOutcome::Lost { .. }) => "watcher lost".to_owned(),
            None => signal_headline(signal),
        },
    };
    let expired = matches!(evidence, Evidence::Expired);
    let verb = if expired { "expired" } else { "fired" };
    let mut body = format!("{name} {verb}: {trigger}");
    if let Some(meta) = meta.filter(|_| !expired) {
        let armer = match &meta.armed_by {
            WakeArmer::Human => "armed from the shell".to_owned(),
            WakeArmer::Agent { handle }
                if task
                    .wake
                    .as_ref()
                    .is_some_and(|target| target.handle == *handle) =>
            {
                "armed by you".to_owned()
            }
            WakeArmer::Agent { handle } => format!("armed by {handle}"),
        };
        body.push_str(&format!(
            ", {armer} at {}",
            meta.armed_at.to_zoned(tz).strftime("%H:%M")
        ));
    }
    if let Evidence::Signal(signal) = evidence {
        let tail = match &signal.watch {
            Some(WatchOutcome::Exited { output, .. } | WatchOutcome::TimedOut { output, .. }) => {
                output.clone()
            }
            Some(WatchOutcome::Lost { detail }) => detail.clone(),
            None => {
                let mut payload = signal.payload.clone();
                payload.insert("signal".to_owned(), Value::String(signal.name.to_string()));
                Value::Object(payload).to_string()
            }
        };
        if !tail.is_empty() {
            body.push('\n');
            body.push_str(&tail);
        }
    }
    if !note.is_empty() {
        body.push_str("\n\n");
        body.push_str(note);
    }
    body
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

fn elapsed_label(elapsed_ms: u64) -> String {
    let seconds = elapsed_ms / 1_000;
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        crate::theme::fmt::duration_label(seconds / 60)
    }
}

#[cfg(test)]
mod tests;
