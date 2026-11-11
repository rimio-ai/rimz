//! Signal vocabulary shared by the loop scheduler and the workflow engine: selectors, the
//! signal itself, a watch's verdict, and how a delivered signal reads in a prompt.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::disk::summary::FileSummary;
use crate::harness::check_run::CheckOutcome;
use crate::store::event::{
    MAX_SIGNAL_NAME_BYTES, SignalEventPayload, SignalName, SignalNameErr, SignalSource,
};

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum SelectorErr {
    #[error("schedule `{name}` has a blank match value for `{key}`")]
    BlankMatch { name: String, key: String },
    #[error("schedule `{name}` has an invalid signal name `{value}`")]
    BadSignal { name: String, value: String },
    #[error(
        "schedule `{name}`: ci.finished was replaced by ci.passed, ci.failed, or 'ci.*'; remove --match conclusion"
    )]
    ObsoleteCiSignal { name: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignalSelector {
    Exact(SignalName),
    Family(String),
}

impl SignalSelector {
    pub fn matches(&self, name: &SignalName) -> bool {
        match self {
            Self::Exact(expected) => expected == name,
            Self::Family(family) => family == name.family(),
        }
    }

    pub(in crate::harness) fn family(&self) -> &str {
        match self {
            Self::Exact(name) => name.family(),
            Self::Family(family) => family,
        }
    }
}

impl fmt::Display for SignalSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exact(name) => name.fmt(f),
            Self::Family(family) => write!(f, "{family}.*"),
        }
    }
}

impl FromStr for SignalSelector {
    type Err = SignalNameErr;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        if let Some(family) = raw.strip_suffix(".*") {
            if raw.len() > MAX_SIGNAL_NAME_BYTES || family.contains('.') {
                return Err(SignalNameErr(raw.to_owned()));
            }
            return family
                .parse::<SignalName>()
                .map(|name| Self::Family(name.as_str().to_owned()));
        }
        raw.parse().map(Self::Exact)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::harness) enum SignalResolution {
    Ignore,
    Skip,
    Deliver,
}

pub(in crate::harness) fn resolve_subscription(
    selector: &SignalSelector,
    matches: &std::collections::BTreeMap<String, String>,
    signal: &Signal,
) -> SignalResolution {
    if selector.family() != signal.name.family()
        || !matches.iter().all(|(key, expected)| {
            signal
                .payload
                .get(key)
                .is_some_and(|value| is_match_wildcard(expected) || match_value(value) == *expected)
        })
    {
        return SignalResolution::Ignore;
    }
    if selector.matches(&signal.name) {
        SignalResolution::Deliver
    } else {
        SignalResolution::Skip
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Signal {
    pub name: SignalName,
    #[serde(default)]
    pub payload: Map<String, Value>,
    pub source: SignalSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch: Option<WatchOutcome>,
}

impl From<&Signal> for SignalEventPayload {
    fn from(signal: &Signal) -> Self {
        Self {
            name: signal.name.clone(),
            payload: signal.payload.clone(),
            source: signal.source,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum WatchVerdict {
    Running {
        elapsed_ms: u64,
    },
    /// A polled watch saw its condition; `line` is the `--grep` match.
    Met {
        elapsed_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        line: Option<String>,
    },
    /// A polled watch's check-in: the condition has not held yet.
    NotMet {
        elapsed_ms: u64,
    },
    Exited {
        code: Option<i32>,
        elapsed_ms: u64,
    },
    TimedOut {
        elapsed_ms: u64,
    },
    Lost {
        detail: String,
        elapsed_ms: u64,
    },
}

impl WatchVerdict {
    pub(in crate::harness) fn is_terminal(&self) -> bool {
        !matches!(self, Self::Running { .. } | Self::NotMet { .. })
    }

    pub fn label(&self) -> String {
        let elapsed = elapsed_label(self.elapsed_ms());
        match self {
            Self::Running { .. } => format!("still running after {elapsed}"),
            Self::Met { line: None, .. } => format!("met after {elapsed}"),
            Self::Met {
                line: Some(line), ..
            } => format!(
                "met after {elapsed}: `{}`",
                crate::theme::fmt::command_preview(line.trim())
            ),
            Self::NotMet { .. } => format!("still not met after {elapsed}"),
            Self::Exited {
                code: Some(code), ..
            } => format!("exit {code} after {elapsed}"),
            Self::Exited { code: None, .. } => format!("killed by signal after {elapsed}"),
            Self::TimedOut { .. } => format!("timed out after {elapsed}"),
            Self::Lost { .. } => format!(
                "watcher died after {elapsed}; the command may still be running or may have died with it"
            ),
        }
    }

    pub fn elapsed_ms(&self) -> u64 {
        match self {
            Self::Running { elapsed_ms }
            | Self::Met { elapsed_ms, .. }
            | Self::NotMet { elapsed_ms }
            | Self::Exited { elapsed_ms, .. }
            | Self::TimedOut { elapsed_ms }
            | Self::Lost { elapsed_ms, .. } => *elapsed_ms,
        }
    }

    fn passed(&self) -> bool {
        matches!(self, Self::Exited { code: Some(0), .. } | Self::Met { .. })
    }
}

pub(in crate::harness) fn elapsed_label(elapsed_ms: u64) -> String {
    let seconds = elapsed_ms / 1_000;
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        crate::theme::fmt::duration_label(seconds / 60)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct WatchOutcome {
    #[serde(flatten)]
    pub verdict: WatchVerdict,
    #[serde(default)]
    pub output: String,
    /// The host path to the full output file.
    pub output_path: Option<PathBuf>,
    #[serde(default)]
    pub summary: FileSummary,
}

impl WatchOutcome {
    pub(in crate::harness) fn measured(verdict: WatchVerdict, output: String, path: &Path) -> Self {
        let summary = FileSummary::measure(path).unwrap_or_else(|err| {
            tracing::warn!(path = %path.display(), error = %err, "measuring wait output");
            FileSummary::default()
        });
        Self {
            verdict,
            output,
            output_path: Some(path.to_path_buf()),
            summary,
        }
    }

    pub(in crate::harness) fn to_check_outcome(&self) -> CheckOutcome {
        let code = match self.verdict {
            WatchVerdict::Exited { code, .. } => code,
            _ => None,
        };
        CheckOutcome::new(
            self.verdict.passed(),
            matches!(self.verdict, WatchVerdict::TimedOut { .. }),
            self.output.clone(),
            code,
        )
    }
}

const MATCH_WILDCARD: &str = "*";

pub(crate) fn is_match_wildcard(value: &str) -> bool {
    value == MATCH_WILDCARD
}

pub(in crate::harness) fn match_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

pub fn parse_signal_selector(
    name: &str,
    raw: &str,
    matches: Option<&std::collections::BTreeMap<String, String>>,
) -> Result<SignalSelector, SelectorErr> {
    if let Some((key, _)) =
        matches.and_then(|matches| matches.iter().find(|(_, value)| value.trim().is_empty()))
    {
        return Err(SelectorErr::BlankMatch {
            name: name.to_owned(),
            key: key.clone(),
        });
    }
    let selector: SignalSelector = raw.parse().map_err(|_| SelectorErr::BadSignal {
        name: name.to_owned(),
        value: raw.to_owned(),
    })?;
    if raw == "ci.finished"
        || (selector.family() == "ci"
            && matches.is_some_and(|matches| matches.contains_key("conclusion")))
    {
        return Err(SelectorErr::ObsoleteCiSignal {
            name: name.to_owned(),
        });
    }
    Ok(selector)
}

pub fn default_signal_match_key(
    selector: &SignalSelector,
    matches: &BTreeMap<String, String>,
) -> Option<&'static str> {
    match selector.family() {
        "ci" | "pr" if !matches.contains_key("path") && !matches.contains_key("branch") => {
            Some("path")
        }
        "team" if !matches.contains_key("team") && !matches.contains_key("instance") => {
            Some("instance")
        }
        _ => None,
    }
}

/// The subject following the name in a delivered signal or event listing.
pub fn signal_subject(
    name: &crate::store::event::SignalName,
    payload: &Map<String, Value>,
) -> String {
    let text = |key: &str| payload.get(key).and_then(Value::as_str);
    let mut headline = String::new();
    match name.family() {
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
    match name.as_str() {
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
    headline.strip_prefix(' ').unwrap_or(&headline).to_owned()
}

/// The lines under the verdict: nothing for a built-in family, whose agent
/// can ask the CLI, and every top-level field for a custom one.
pub(in crate::harness) fn signal_details(signal: &Signal) -> Vec<String> {
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
pub(in crate::harness) fn one_line(text: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_verdicts_share_labels_and_check_semantics() {
        for (verdict, label, passed, timed_out, code) in [
            (
                WatchVerdict::Running {
                    elapsed_ms: 1_800_000,
                },
                "still running after 30m",
                false,
                false,
                None,
            ),
            (
                WatchVerdict::NotMet {
                    elapsed_ms: 1_800_000,
                },
                "still not met after 30m",
                false,
                false,
                None,
            ),
            (
                WatchVerdict::Met {
                    elapsed_ms: 3_000,
                    line: None,
                },
                "met after 3s",
                true,
                false,
                None,
            ),
            (
                WatchVerdict::Met {
                    elapsed_ms: 3_000,
                    line: Some("  listening on :3000\n".to_owned()),
                },
                "met after 3s: `listening on :3000`",
                true,
                false,
                None,
            ),
            (
                WatchVerdict::Exited {
                    code: Some(0),
                    elapsed_ms: 3_000,
                },
                "exit 0 after 3s",
                true,
                false,
                Some(0),
            ),
            (
                WatchVerdict::Exited {
                    code: Some(3),
                    elapsed_ms: 720_000,
                },
                "exit 3 after 12m",
                false,
                false,
                Some(3),
            ),
            (
                WatchVerdict::Exited {
                    code: None,
                    elapsed_ms: 3_000,
                },
                "killed by signal after 3s",
                false,
                false,
                None,
            ),
            (
                WatchVerdict::TimedOut {
                    elapsed_ms: 3_540_000,
                },
                "timed out after 59m",
                false,
                true,
                None,
            ),
            (
                WatchVerdict::Lost {
                    detail: "diagnostic".to_owned(),
                    elapsed_ms: 180_000,
                },
                "watcher died after 3m; the command may still be running or may have died with it",
                false,
                false,
                None,
            ),
        ] {
            assert_eq!(verdict.label(), label);
            assert_eq!(
                verdict.is_terminal(),
                !matches!(
                    verdict,
                    WatchVerdict::Running { .. } | WatchVerdict::NotMet { .. }
                )
            );
            assert_eq!(verdict.passed(), passed);
            let outcome = WatchOutcome {
                verdict,
                output: "actual tail".to_owned(),
                output_path: Some(PathBuf::from("/state/out/planner/wait.output")),
                summary: FileSummary {
                    bytes: 11,
                    lines: 1,
                    tokens: 2,
                },
            };
            let encoded = serde_json::to_string(&outcome).unwrap();
            assert_eq!(
                serde_json::from_str::<WatchOutcome>(&encoded).unwrap(),
                outcome
            );
            let mut legacy = serde_json::to_value(&outcome).unwrap();
            legacy.as_object_mut().unwrap().remove("summary");
            assert_eq!(
                serde_json::from_value::<WatchOutcome>(legacy)
                    .unwrap()
                    .summary,
                FileSummary::default()
            );
            let mut pre_tokens = serde_json::to_value(&outcome).unwrap();
            pre_tokens["summary"]
                .as_object_mut()
                .unwrap()
                .remove("tokens");
            assert_eq!(
                serde_json::from_value::<WatchOutcome>(pre_tokens)
                    .unwrap()
                    .summary,
                FileSummary {
                    tokens: 0,
                    ..outcome.summary
                }
            );
            let check = outcome.to_check_outcome();
            assert_eq!(check.output(), "actual tail");
            assert_eq!(check.timed_out(), timed_out);
            assert_eq!(check.code(), code);
        }
    }

    #[test]
    fn signal_names_pin_the_public_grammar() {
        for valid in [
            "ci.failed",
            "ci.*",
            "deploy.finished",
            "a.b.c",
            "deploy_done",
        ] {
            let selector = valid.parse::<SignalSelector>().unwrap();
            assert_eq!(selector.to_string(), valid);
            assert_eq!(
                selector.to_string().parse::<SignalSelector>().unwrap(),
                selector
            );
        }
        for invalid in ["*", "a.b.*", "a*", "A.*", ".*", "a..*"] {
            assert!(invalid.parse::<SignalSelector>().is_err(), "{invalid}");
        }
    }

    #[test]
    fn signal_match_values_reject_blanks_but_accept_wildcards() {
        for value in ["", " \t"] {
            let matches =
                std::collections::BTreeMap::from([("branch".to_owned(), value.to_owned())]);
            let err = parse_signal_selector("task", "pr.conflicted", Some(&matches)).unwrap_err();
            assert!(err.to_string().contains("task"));
            assert!(err.to_string().contains("branch"));
        }
        let matches = std::collections::BTreeMap::from([("branch".to_owned(), "*".to_owned())]);
        assert!(parse_signal_selector("task", "pr.conflicted", Some(&matches)).is_ok());
    }
}
