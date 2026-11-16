//! Pure forge-signal derivation from consecutive PR cache publications: checkout rows, and the
//! branch rows that live signal subscriptions and conditions demand.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use super::{RepoGroup, Target};
use crate::forge::pr_state::{
    BranchRow, PrLink, PrQueueFact, PrStateCache, SettledMergeability, TargetStamp,
};
use crate::forge::{ForgeSignal, RemoteRepo};
use crate::store::snapshot::{WorktreeCi, WorktreePrState};

pub(super) fn transitions(
    prior: &PrStateCache,
    next: &PrStateCache,
    groups: &BTreeMap<String, RepoGroup>,
    failed: &BTreeSet<String>,
) -> Vec<(&'static str, Map<String, Value>)> {
    let mut signals = Vec::new();
    for (path, next_link) in &next.states {
        if failed.contains(path) {
            continue;
        }
        let Some(stamp) = continuous_target(prior, next, path) else {
            continue;
        };
        let Some(repo) = next.path_repos.get(path).map(String::as_str) else {
            continue;
        };
        let remote = target_for_path(groups, repo, path).map(|target| &target.remote);
        if !stamp.owns_link(next_link) {
            continue;
        }
        let prior_link = prior.states.get(path).filter(|link| stamp.owns_link(link));
        let source = Source {
            path: Some(path),
            branch: &stamp.branch,
            repo,
            remote,
        };
        for signal in pr_moves(prior_link, next_link) {
            signals.push((
                signal.as_str(),
                payload(next, &source, Some(next_link), Some(signal)),
            ));
        }
        let Some(prior_link) = prior_link else {
            continue;
        };
        if let Some(name) = final_verdict_name(next_link.ci)
            && prior_link.ci != next_link.ci
        {
            signals.push((name, payload(next, &source, Some(next_link), None)));
        }
    }

    for (path, next_ci) in &next.branch_ci {
        if failed.contains(path) {
            continue;
        }
        let Some(stamp) = continuous_target(prior, next, path) else {
            continue;
        };
        let Some(repo) = next.path_repos.get(path).map(String::as_str) else {
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
        let source = Source {
            path: Some(path),
            branch: &stamp.branch,
            repo,
            remote: target.map(|target| &target.remote),
        };
        signals.push((name, payload(next, &source, None, None)));
    }

    for (repo, rows) in &next.branches {
        for (branch, row) in rows {
            branch_row_signals(prior, next, groups, repo, branch, row, &mut signals);
        }
    }
    signals
}

/// A demanded branch's own changes, for the subscriptions its payload matches. Only a row read
/// successfully in both passes emits. A signal a checkout on the same repository and branch
/// already emitted this pass for the same change (the same pull request, or the same verdict
/// at the same commit) shadows the row's, so each change fires once; a checkout reading
/// another commit, or whose reading failed, shadows nothing. A row commit that is unknown
/// proves no shadow, since a signal fired twice costs less than one never fired.
fn branch_row_signals(
    prior: &PrStateCache,
    next: &PrStateCache,
    groups: &BTreeMap<String, RepoGroup>,
    repo: &str,
    branch: &str,
    row: &BranchRow,
    signals: &mut Vec<(&'static str, Map<String, Value>)>,
) {
    let Some(prior_row) = prior
        .branches
        .get(repo)
        .and_then(|rows| rows.get(branch))
        .filter(|prior_row| !prior_row.unread && !row.unread)
    else {
        return;
    };
    let Some(target) = groups.get(repo).and_then(|group| {
        group
            .targets
            .iter()
            .find(|target| target.branch_only && target.branch == branch)
    }) else {
        return;
    };
    let source = Source {
        path: None,
        branch,
        repo,
        remote: Some(&target.remote),
    };
    if !target.trunk_row
        && let Some(link) = &row.pr
    {
        for signal in pr_moves(prior_row.pr.as_ref(), link) {
            let number = link.number.map(Value::from);
            if !checkout_emitted(signals, &source, signal.as_str(), "number", number) {
                signals.push((
                    signal.as_str(),
                    payload(next, &source, Some(link), Some(signal)),
                ));
            }
        }
    }
    let (verdict_link, ci) = branch_ci(row);
    if let Some(name) = final_verdict_name(ci)
        && branch_ci(prior_row).1 != ci
    {
        let head = match verdict_link {
            Some(link) => link.ci_sha.as_ref(),
            None => row.ci_sha.as_ref(),
        };
        let head = head
            .filter(|head| !head.is_empty())
            .cloned()
            .map(Value::from);
        if !checkout_emitted(signals, &source, name, "head", head) {
            signals.push((name, payload(next, &source, verdict_link, None)));
        }
    }
}

/// Whether a checkout on `source`'s repository and branch already emitted `name` this pass
/// with payload `key` equal to `value`; an unknown `value` matches nothing.
fn checkout_emitted(
    signals: &[(&'static str, Map<String, Value>)],
    source: &Source<'_>,
    name: &str,
    key: &str,
    value: Option<Value>,
) -> bool {
    let Some(value) = value else {
        return false;
    };
    signals.iter().any(|(emitted, payload)| {
        *emitted == name
            && payload.contains_key("path")
            && payload.get("repo").and_then(Value::as_str) == Some(source.repo)
            && payload.get("branch").and_then(Value::as_str) == Some(source.branch)
            && payload.get(key) == Some(&value)
    })
}

/// The CI verdict a `ci[branch]` condition reads: an open or merged pull request's, then the
/// branch head's, with the link that carries it.
fn branch_ci(row: &BranchRow) -> (Option<&PrLink>, Option<WorktreeCi>) {
    let link = row.pr.as_ref().filter(|link| {
        matches!(link.state, WorktreePrState::Open | WorktreePrState::Merged) && link.ci.is_some()
    });
    match link {
        Some(link) => (Some(link), link.ci),
        None => (None, row.ci),
    }
}

/// The pull request signals a reading moved into since the last successful one.
fn pr_moves(prior: Option<&PrLink>, next: &PrLink) -> Vec<ForgeSignal> {
    let mut moves = Vec::new();
    if next.state == WorktreePrState::Open
        && !prior
            .is_some_and(|link| link.state == WorktreePrState::Open && link.number == next.number)
    {
        moves.push(ForgeSignal::PrOpened);
    }
    for signal in [
        ForgeSignal::PrBehind,
        ForgeSignal::PrConflicted,
        ForgeSignal::PrQueued,
        ForgeSignal::PrDequeued,
    ] {
        if let Some(key) = open_key(Some(next), signal)
            && Some(key) != open_key(prior, signal)
        {
            moves.push(signal);
        }
    }
    if prior.is_some_and(|link| link.state == WorktreePrState::Open) {
        match next.state {
            WorktreePrState::Merged => moves.push(ForgeSignal::PrMerged),
            WorktreePrState::Closed => moves.push(ForgeSignal::PrClosed),
            WorktreePrState::Open => {}
        }
    }
    moves
}

/// What an open-PR signal re-emits on: the head it was read at for an adverse
/// state, the forge's own timestamp for a merge-queue entry or removal.
fn open_key(link: Option<&PrLink>, signal: ForgeSignal) -> Option<&str> {
    let facts = link
        .filter(|link| link.state == WorktreePrState::Open)?
        .open
        .as_ref()?;
    match (signal, &facts.queue) {
        (ForgeSignal::PrBehind, _) if facts.behind_by.is_some_and(|behind| behind > 0) => {
            Some(facts.head.as_str())
        }
        (ForgeSignal::PrConflicted, _) => match &facts.mergeability {
            Some(SettledMergeability::Conflicting(head)) => Some(head.as_str()),
            _ => None,
        },
        (ForgeSignal::PrQueued, Some(PrQueueFact::Queued { at }))
        | (ForgeSignal::PrDequeued, Some(PrQueueFact::Dequeued { at, .. })) => Some(at.as_str()),
        _ => None,
    }
    .filter(|key| !key.is_empty())
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

/// The target's stamp when this pass and the prior one both hold a successful
/// reading under it: `target_seen` marks a target's last successful reading.
fn continuous_target<'a>(
    prior: &PrStateCache,
    next: &'a PrStateCache,
    path: &str,
) -> Option<&'a TargetStamp> {
    let next_stamp = next.target_seen.get(path)?;
    (prior.target_seen.get(path) == Some(next_stamp)).then_some(next_stamp)
}

fn final_verdict_name(ci: Option<WorktreeCi>) -> Option<&'static str> {
    match ci {
        Some(WorktreeCi::Passing) => Some(ForgeSignal::CiPassed.as_str()),
        Some(WorktreeCi::Failing) => Some(ForgeSignal::CiFailed.as_str()),
        Some(WorktreeCi::Pending) | None => None,
    }
}

/// Where a signal's reading came from: a checkout `path`, or a branch row without one.
struct Source<'a> {
    path: Option<&'a str>,
    branch: &'a str,
    repo: &'a str,
    remote: Option<&'a RemoteRepo>,
}

fn payload(
    cache: &PrStateCache,
    source: &Source<'_>,
    link: Option<&PrLink>,
    signal: Option<ForgeSignal>,
) -> Map<String, Value> {
    let remote = source.remote;
    let mut payload = Map::from_iter([
        ("branch".to_owned(), Value::String(source.branch.to_owned())),
        ("repo".to_owned(), Value::String(source.repo.to_owned())),
    ]);
    if let Some(path) = source.path {
        payload.insert("path".to_owned(), Value::String(path.to_owned()));
    }
    if let Some(link) = link {
        if let Some(number) = link.number {
            payload.insert("number".to_owned(), Value::Number(number.into()));
        }
        if let Some(url) = &link.url {
            payload.insert("url".to_owned(), Value::String(url.clone()));
        }
    }
    let head = match (link, signal) {
        (Some(link), None | Some(ForgeSignal::CiPassed | ForgeSignal::CiFailed)) => {
            link.ci_sha.as_ref()
        }
        _ => source.path.and_then(|path| cache.head_seen.get(path)),
    };
    if let Some(head) = head.filter(|head| !head.is_empty()) {
        payload.insert("head".to_owned(), Value::String(head.clone()));
        if let Some(url) = remote.and_then(|remote| remote.checks_web_url(head)) {
            payload.insert("checks_url".to_owned(), Value::String(url));
        }
    }
    let state = match signal {
        Some(ForgeSignal::PrMerged) => Some("merged"),
        Some(ForgeSignal::PrClosed) => Some("closed"),
        Some(
            ForgeSignal::PrOpened
            | ForgeSignal::PrBehind
            | ForgeSignal::PrConflicted
            | ForgeSignal::PrQueued
            | ForgeSignal::PrDequeued,
        ) => Some("open"),
        Some(ForgeSignal::CiPassed | ForgeSignal::CiFailed) | None => None,
    };
    if let Some(state) = state {
        payload.insert("state".to_owned(), Value::String(state.to_owned()));
    }
    if let Some(signal) = signal
        && let Some(key) = open_key(link, signal)
        && let Some(facts) = link.and_then(|link| link.open.as_ref())
    {
        let mut insert = |name: &str, value: &str| {
            payload.insert(name.to_owned(), Value::String(value.to_owned()));
        };
        match (signal, &facts.queue) {
            (ForgeSignal::PrQueued, _) => {
                insert("pr_head", &facts.head);
                insert("queued_at", key);
            }
            (ForgeSignal::PrDequeued, Some(PrQueueFact::Dequeued { reason, commit, .. })) => {
                insert("pr_head", &facts.head);
                insert("dequeued_at", key);
                if let Some(reason) = reason {
                    insert("reason", reason);
                }
                if let Some(url) = commit
                    .as_deref()
                    .and_then(|commit| remote?.checks_web_url(commit))
                {
                    insert("queue_checks_url", &url);
                }
            }
            // An adverse state's key is the head it was read at.
            _ => insert("pr_head", key),
        }
        if let Some(base) = &facts.base {
            insert("base", base);
        }
        if signal == ForgeSignal::PrBehind
            && let Some(behind) = facts.behind_by
        {
            payload.insert("behind_by".to_owned(), Value::Number(behind.into()));
        }
    }
    payload
}
