//! Pure forge-signal derivation from consecutive PR cache publications.

use serde_json::{Map, Value};

use super::{PrLink, PrStateCache, TargetStamp};
use crate::harness::schedule::signal::{Signal, SignalSource};
use crate::store::snapshot::{WorktreePrCi, WorktreePrState};

pub(super) fn transitions(prior: &PrStateCache, next: &PrStateCache) -> Vec<Signal> {
    let mut signals = Vec::new();
    for (path, next_link) in &next.states {
        let Some(stamp) = continuous_target(prior, next, path) else {
            continue;
        };
        let Some(repo) = successful_repo(next, path) else {
            continue;
        };
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
                        None,
                    ),
                ));
            }
        }
        if let Some(conclusion) = finished_conclusion(next_link.ci)
            && prior_link.ci != next_link.ci
        {
            signals.push(signal(
                "ci.finished",
                payload(
                    next,
                    path,
                    stamp,
                    repo,
                    Some(next_link),
                    None,
                    Some(conclusion),
                ),
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
        let Some(conclusion) = finished_conclusion(Some(*next_ci)) else {
            continue;
        };
        if prior.branch_ci.get(path) == Some(next_ci) {
            continue;
        }
        signals.push(signal(
            "ci.finished",
            payload(next, path, stamp, repo, None, None, Some(conclusion)),
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

fn finished_conclusion(ci: Option<WorktreePrCi>) -> Option<&'static str> {
    match ci {
        Some(WorktreePrCi::Passing) => Some("success"),
        Some(WorktreePrCi::Failing) => Some("failure"),
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
    conclusion: Option<&str>,
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
    }
    if let Some(state) = state {
        let state = match state {
            WorktreePrState::Merged => "merged",
            WorktreePrState::Closed => "closed",
            WorktreePrState::Open => "open",
        };
        payload.insert("state".to_owned(), Value::String(state.to_owned()));
    }
    if let Some(conclusion) = conclusion {
        payload.insert(
            "conclusion".to_owned(),
            Value::String(conclusion.to_owned()),
        );
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
