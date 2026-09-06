//! Pure forge-signal derivation from consecutive PR cache publications.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{PrLink, PrStateCache, RepoGroup, TargetStamp};
use crate::forge::RemoteRepo;
use crate::harness::schedule::signal::Signal;
use crate::store::event::SignalSource;
use crate::store::snapshot::{WorktreePrCi, WorktreePrState};

pub(super) fn transitions(
    prior: &PrStateCache,
    next: &PrStateCache,
    groups: &BTreeMap<String, RepoGroup>,
) -> Vec<Signal> {
    let mut signals = Vec::new();
    for (path, next_link) in &next.states {
        let Some(stamp) = continuous_target(prior, next, path) else {
            continue;
        };
        let Some(repo) = successful_repo(next, path) else {
            continue;
        };
        let remote = groups.get(repo).and_then(|group| {
            group
                .targets
                .iter()
                .find(|target| target.path == *path)
                .map(|target| &target.remote)
        });
        if !stamp.owns_link(next_link) {
            continue;
        }
        let Some(prior_link) = prior.states.get(path) else {
            continue;
        };
        if !stamp.owns_link(prior_link) {
            continue;
        }
        if prior_link.state == WorktreePrState::Open {
            let name = match next_link.state {
                WorktreePrState::Merged => Some("pr.merged"),
                WorktreePrState::Closed => Some("pr.closed"),
                WorktreePrState::Open => None,
            };
            if let Some(name) = name {
                signals.push(signal(
                    name,
                    payload(
                        next,
                        path,
                        stamp,
                        repo,
                        Some(next_link),
                        Some(next_link.state),
                        remote,
                    ),
                ));
            }
        }
        if let Some(name) = final_verdict_name(next_link.ci)
            && prior_link.ci != next_link.ci
        {
            signals.push(signal(
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
        let remote = groups.get(repo).and_then(|group| {
            group
                .targets
                .iter()
                .find(|target| target.path == *path)
                .map(|target| &target.remote)
        });
        signals.push(signal(
            name,
            payload(next, path, stamp, repo, None, None, remote),
        ));
    }
    signals
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

fn final_verdict_name(ci: Option<WorktreePrCi>) -> Option<&'static str> {
    match ci {
        Some(WorktreePrCi::Passing) => Some("ci.passed"),
        Some(WorktreePrCi::Failing) => Some("ci.failed"),
        Some(WorktreePrCi::Pending) | None => None,
    }
}

fn payload(
    cache: &PrStateCache,
    path: &str,
    stamp: &TargetStamp,
    repo: &str,
    link: Option<&PrLink>,
    state: Option<WorktreePrState>,
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
    if let Some(state) = state {
        let state = match state {
            WorktreePrState::Merged => "merged",
            WorktreePrState::Closed => "closed",
            WorktreePrState::Open => "open",
        };
        payload.insert("state".to_owned(), Value::String(state.to_owned()));
    }
    payload
}

fn signal(name: &str, payload: Map<String, Value>) -> Signal {
    Signal {
        name: name.parse().expect("static forge signal name is valid"),
        payload,
        source: SignalSource::Forge,
        watch: None,
    }
}
