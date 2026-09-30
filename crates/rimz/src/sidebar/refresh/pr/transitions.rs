//! Pure forge-signal derivation from consecutive PR cache publications.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{RepoGroup, Target};
use crate::forge::pr_state::{PrLink, PrStateCache, SettledMergeability, TargetStamp};
use crate::forge::{ForgeSignal, RemoteRepo};
use crate::store::snapshot::{WorktreeCi, WorktreePrState};

pub(super) fn transitions(
    prior: &PrStateCache,
    next: &PrStateCache,
    groups: &BTreeMap<String, RepoGroup>,
) -> Vec<(&'static str, Map<String, Value>)> {
    let mut signals = Vec::new();
    for (path, next_link) in &next.states {
        let Some(stamp) = continuous_target(prior, next, path) else {
            continue;
        };
        let Some(repo) = successful_repo(next, path) else {
            continue;
        };
        let remote = target_for_path(groups, repo, path).map(|target| &target.remote);
        if !stamp.owns_link(next_link) {
            continue;
        }
        let prior_link = prior.states.get(path).filter(|link| stamp.owns_link(link));
        if next_link.state == WorktreePrState::Open
            && prior.repos.get(repo).is_some_and(|probe| probe.ok)
            && !prior_link.is_some_and(|link| {
                link.state == WorktreePrState::Open && link.number == next_link.number
            })
        {
            signals.push((
                ForgeSignal::PrOpened.as_str(),
                payload(
                    next,
                    path,
                    stamp,
                    repo,
                    Some(next_link),
                    Some(ForgeSignal::PrOpened),
                    remote,
                ),
            ));
        }
        for signal in [ForgeSignal::PrBehind, ForgeSignal::PrConflicted] {
            if let Some(key) = open_key(Some(next_link), signal)
                && Some(key) != open_key(prior_link, signal)
            {
                signals.push((
                    signal.as_str(),
                    payload(
                        next,
                        path,
                        stamp,
                        repo,
                        Some(next_link),
                        Some(signal),
                        remote,
                    ),
                ));
            }
        }
        let Some(prior_link) = prior_link else {
            continue;
        };
        if prior_link.state == WorktreePrState::Open {
            let signal = match next_link.state {
                WorktreePrState::Merged => Some(ForgeSignal::PrMerged),
                WorktreePrState::Closed => Some(ForgeSignal::PrClosed),
                WorktreePrState::Open => None,
            };
            if let Some(signal) = signal {
                signals.push((
                    signal.as_str(),
                    payload(
                        next,
                        path,
                        stamp,
                        repo,
                        Some(next_link),
                        Some(signal),
                        remote,
                    ),
                ));
            }
        }
        if let Some(name) = final_verdict_name(next_link.ci)
            && prior_link.ci != next_link.ci
        {
            signals.push((
                name,
                payload(next, path, stamp, repo, Some(next_link), None, remote),
            ));
        }
    }

    for (path, next_ci) in &next.branch_ci {
        let Some(stamp) = continuous_target(prior, next, path) else {
            continue;
        };
        let Some(repo) = successful_repo(next, path) else {
            continue;
        };
        if prior.states.contains_key(path) || next.states.contains_key(path) {
            continue;
        }
        let Some(name) = final_verdict_name(Some(*next_ci)) else {
            continue;
        };
        if prior.branch_ci.get(path) == Some(next_ci) {
            continue;
        }
        let target = target_for_path(groups, repo, path);
        if target.is_some_and(|target| !target.trunk && target.inherited_head) {
            continue;
        }
        let remote = target.map(|target| &target.remote);
        signals.push((name, payload(next, path, stamp, repo, None, None, remote)));
    }
    signals
}

fn open_key(link: Option<&PrLink>, signal: ForgeSignal) -> Option<&str> {
    let facts = link
        .filter(|link| link.state == WorktreePrState::Open)?
        .open
        .as_ref()?;
    match signal {
        ForgeSignal::PrBehind if facts.behind_by.is_some_and(|behind| behind > 0) => {
            Some(facts.head.as_str())
        }
        ForgeSignal::PrConflicted => match &facts.mergeability {
            Some(SettledMergeability::Conflicting(head)) => Some(head.as_str()),
            _ => None,
        },
        _ => None,
    }
    .filter(|head| !head.is_empty())
}

fn target_for_path<'a>(
    groups: &'a BTreeMap<String, RepoGroup>,
    repo: &str,
    path: &str,
) -> Option<&'a Target> {
    groups
        .get(repo)?
        .targets
        .iter()
        .find(|target| target.path == path)
}

fn continuous_target<'a>(
    prior: &PrStateCache,
    next: &'a PrStateCache,
    path: &str,
) -> Option<&'a TargetStamp> {
    let next_stamp = next.target_seen.get(path)?;
    (prior.target_seen.get(path) == Some(next_stamp)).then_some(next_stamp)
}

fn successful_repo<'a>(cache: &'a PrStateCache, path: &str) -> Option<&'a str> {
    let repo = cache.path_repos.get(path)?;
    cache
        .repos
        .get(repo)
        .is_some_and(|probe| probe.ok)
        .then_some(repo)
}

fn final_verdict_name(ci: Option<WorktreeCi>) -> Option<&'static str> {
    match ci {
        Some(WorktreeCi::Passing) => Some(ForgeSignal::CiPassed.as_str()),
        Some(WorktreeCi::Failing) => Some(ForgeSignal::CiFailed.as_str()),
        Some(WorktreeCi::Pending) | None => None,
    }
}

fn payload(
    cache: &PrStateCache,
    path: &str,
    stamp: &TargetStamp,
    repo: &str,
    link: Option<&PrLink>,
    signal: Option<ForgeSignal>,
    remote: Option<&RemoteRepo>,
) -> Map<String, Value> {
    let mut payload = Map::from_iter([
        ("path".to_owned(), Value::String(path.to_owned())),
        ("branch".to_owned(), Value::String(stamp.branch.clone())),
        ("repo".to_owned(), Value::String(repo.to_owned())),
    ]);
    if let Some(link) = link {
        if let Some(number) = link.number {
            payload.insert("number".to_owned(), Value::Number(number.into()));
        }
        if let Some(url) = &link.url {
            payload.insert("url".to_owned(), Value::String(url.clone()));
        }
    }
    if let Some(head) = cache.head_seen.get(path).filter(|head| !head.is_empty()) {
        payload.insert("head".to_owned(), Value::String(head.clone()));
        if let Some(url) = remote.and_then(|remote| remote.checks_web_url(head)) {
            payload.insert("checks_url".to_owned(), Value::String(url));
        }
    }
    let state = match signal {
        Some(ForgeSignal::PrMerged) => Some("merged"),
        Some(ForgeSignal::PrClosed) => Some("closed"),
        Some(ForgeSignal::PrOpened | ForgeSignal::PrBehind | ForgeSignal::PrConflicted) => {
            Some("open")
        }
        _ => None,
    };
    if let Some(state) = state {
        payload.insert("state".to_owned(), Value::String(state.to_owned()));
    }
    if let Some(signal) = signal
        && let Some(head) = open_key(link, signal)
        && let Some(facts) = link.and_then(|link| link.open.as_ref())
    {
        payload.insert("pr_head".to_owned(), Value::String(head.to_owned()));
        if let Some(base) = &facts.base {
            payload.insert("base".to_owned(), Value::String(base.clone()));
        }
        if signal == ForgeSignal::PrBehind
            && let Some(behind) = facts.behind_by
        {
            payload.insert("behind_by".to_owned(), Value::Number(behind.into()));
        }
    }
    payload
}
