//! Deadline answers from the room's current forge cache, not its signal history.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::config::TaskEntry;
use crate::disk::paths::RuntimePaths;
use crate::harness::schedule::Trigger;
use crate::harness::schedule::run_log::SignalRecord;
use crate::harness::schedule::signal::SignalSelector;
use crate::sidebar::refresh::pr::{PrStateCache, read_pr_state_cache};
use crate::store::snapshot::{WorktreePrCi, WorktreePrState};

pub(super) struct ForgeView {
    pub headline: String,
    pub label: String,
}

pub(super) enum WaitStatus {
    Answered { label: String, signal: SignalRecord },
    Open(Option<ForgeView>),
}

pub(super) fn resolve(trigger: &Trigger, runtime: &RuntimePaths) -> WaitStatus {
    let Trigger::Signal { selector, matches } = trigger else {
        return WaitStatus::Open(None);
    };
    if !matches!(selector.family(), "ci" | "pr") {
        return WaitStatus::Open(None);
    }
    from_cache(
        selector,
        matches,
        &read_pr_state_cache(&runtime.pr_state_path()),
    )
}

fn from_cache(
    selector: &SignalSelector,
    matches: &BTreeMap<String, String>,
    cache: &PrStateCache,
) -> WaitStatus {
    if matches
        .keys()
        .any(|key| !matches!(key.as_str(), "path" | "branch"))
    {
        return WaitStatus::Open(None);
    }
    let branch = matches.get("branch");
    let (path, link) = if let Some(path) = matches.get("path") {
        let link = cache.states.get(path);
        if let Some(branch) = branch
            && link.and_then(|link| link.branch.as_ref()) != Some(branch)
        {
            return WaitStatus::Open(None);
        }
        (path, link)
    } else if let Some(branch) = branch {
        let mut links = cache
            .states
            .iter()
            .filter(|(_, link)| link.branch.as_ref() == Some(branch));
        let Some((path, link)) = links.next() else {
            return unknown(branch);
        };
        if links.next().is_some() {
            return WaitStatus::Open(None);
        }
        (path, Some(link))
    } else {
        return WaitStatus::Open(None);
    };
    let ci = match link {
        Some(link) => link.ci,
        None => cache.branch_ci.get(path).copied(),
    };
    if link.is_none() && ci.is_none() {
        return unknown(path);
    }
    let mut headline = link
        .and_then(|link| link.branch.clone())
        .unwrap_or_else(|| path.clone());
    let mut payload = Map::from_iter([("path".to_owned(), Value::String(path.clone()))]);
    if let Some(link) = link {
        if let Some(branch) = &link.branch {
            payload.insert("branch".to_owned(), Value::String(branch.clone()));
        }
        if let Some(number) = link.number {
            headline.push_str(&format!(" (PR #{number})"));
            payload.insert("number".to_owned(), Value::Number(number.into()));
        }
    }
    let (state, current) = if selector.family() == "ci" {
        match ci {
            Some(WorktreePrCi::Pending) => ("ci pending", None),
            Some(WorktreePrCi::Passing) => ("ci passing", Some("ci.passed")),
            Some(WorktreePrCi::Failing) => ("ci failing", Some("ci.failed")),
            None => ("no CI seen", None),
        }
    } else {
        match link.map(|link| link.state) {
            Some(WorktreePrState::Open) => ("pr open", None),
            Some(WorktreePrState::Closed) => ("pr closed", Some("pr.closed")),
            Some(WorktreePrState::Merged) => ("pr merged", Some("pr.merged")),
            None => ("no PR seen", None),
        }
    };
    let mut label = format!("{state} on {headline}");
    if let Some(current) = current {
        if matches!(selector, SignalSelector::Exact(name) if name.as_str() != current) {
            return WaitStatus::Answered {
                label,
                signal: SignalRecord {
                    // The state matches above return fixed, valid signal names.
                    name: current.parse().expect("forge verdict signal name"),
                    payload,
                },
            };
        }
        label.push_str("; no matching transition received");
    }
    WaitStatus::Open(Some(ForgeView { headline, label }))
}

fn unknown(scope: &str) -> WaitStatus {
    WaitStatus::Open(Some(ForgeView {
        headline: scope.to_owned(),
        label: format!("no PR or CI seen on {scope}"),
    }))
}

pub(super) fn rearm_command(entry: &TaskEntry) -> Result<String, shlex::QuoteError> {
    let mut args = vec!["rimz".to_owned(), "wake".to_owned(), "--signal".to_owned()];
    args.extend(entry.signal.iter().cloned());
    for (key, value) in entry.matches.iter().flatten() {
        args.extend(["--match".to_owned(), format!("{key}={value}")]);
    }
    if let Some(timeout) = entry.timeout.as_ref().filter(|timeout| *timeout != "59m") {
        args.extend(["--timeout".to_owned(), timeout.clone()]);
    }
    if let Some(path) = &entry.prompt_file {
        args.extend(["--prompt-file".to_owned(), path.display().to_string()]);
    }
    shlex::try_join(args.iter().map(String::as_str))
}

#[cfg(test)]
mod tests;
